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
use super::profiles::{ALL_WORKER_ROLES, worker_catalog_prompt};
use super::signals::{
    CapabilityArea, PLAN_ONLY_CUES, READ_INTENT_CUES, contains_any, matched_capability_areas,
};
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
        InteractionMode::Task => {
            return forced_task_decision(message);
        }
        InteractionMode::Auto => {}
    }

    let deterministic = route_deterministically(message);
    if let Some(llm) = llm
        && let Ok(decision) = route_with_llm(&llm, message).await
    {
        return reconcile_llm_with_deterministic(decision, deterministic);
    }

    deterministic
}

fn forced_decision(route: TaskRouteKind, message: &str, reason: &str) -> TaskRouteDecision {
    let classification = HeuristicClassifier::new().classify(message);
    TaskRouteDecision {
        route,
        confidence: 1.0,
        reason: reason.to_string(),
        suggested_workers: select_workers(route, classification.inferred_profile, message),
        classification,
    }
}

fn forced_task_decision(message: &str) -> TaskRouteDecision {
    let route = if looks_like_parallel_readonly(message) {
        TaskRouteKind::ParallelReadonlyDelegation
    } else {
        TaskRouteKind::ComplexRuntime
    };
    forced_decision(route, message, "forced task mode")
}

fn reconcile_llm_with_deterministic(
    mut llm: TaskRouteDecision,
    deterministic: TaskRouteDecision,
) -> TaskRouteDecision {
    if deterministic.route == TaskRouteKind::PlanOnly {
        return deterministic;
    }
    if deterministic.route == TaskRouteKind::ParallelReadonlyDelegation
        && llm.route != TaskRouteKind::PlanOnly
    {
        let llm_route = llm.route.as_str();
        let llm_reason = llm.reason;
        llm.route = TaskRouteKind::ParallelReadonlyDelegation;
        llm.confidence = llm.confidence.max(deterministic.confidence);
        llm.reason = format!(
            "deterministic read-only fanout override; llm_route={llm_route}; llm_reason={llm_reason}"
        );
        if llm.suggested_workers.is_empty() {
            llm.suggested_workers = deterministic.suggested_workers;
        }
    }
    llm
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
            message,
        ),
        classification,
    })
}

fn router_system_prompt() -> String {
    format!(
        r#"You route a user's message for a local AI coding assistant runtime.
Return ONLY JSON:
{{
  "route": "normal_chat" | "plan_only" | "complex_runtime" | "parallel_readonly_delegation" | "background_task" | "direct_edit",
  "domain_profile": "general" | "ai_coding" | "academic_research" | "data_analysis" | "medical_research",
  "confidence": number between 0 and 1,
  "reason": string,
  "suggested_workers": string[]
}}

EKO's main domains are AI coding, academic research, data processing/analysis, and medical research. Choose "parallel_readonly_delegation" for broad read-only work that benefits from multiple workers in any of these domains: project architecture analysis, codebase review, literature exploration, evidence review, dataset profiling, analysis-method review, clinical evidence review, safety review, or comparing several candidate root causes.

Domains are context labels, not worker boundaries. A medical or academic task may need data-analysis workers; a data task may need coding workers; a coding task may need literature/evidence workers. Suggested workers are not a fixed count. Pick the smallest useful cross-domain set from this capability catalog:
{catalog}

Choose "plan_only" when the user explicitly asks for a plan or says not to execute.
Choose "complex_runtime" for multi-step work that may include edits, verification, or review gates.
Choose "normal_chat" for small questions, explanations, or single-file/simple answers.
Choose "direct_edit" only for tiny obvious edits.
Choose "background_task" for long detached tasks the user expects to run asynchronously."#,
        catalog = worker_catalog_prompt()
    )
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
        suggested_workers: select_workers(route, classification.inferred_profile, message),
        classification,
    }
}

fn looks_like_plan_only(message: &str) -> bool {
    let lower = message.to_lowercase();
    contains_any(&lower, PLAN_ONLY_CUES)
}

fn looks_like_parallel_readonly(message: &str) -> bool {
    let lower = message.to_lowercase();
    contains_any(&lower, READ_INTENT_CUES)
        && !matched_capability_areas(DomainProfile::General, message).is_empty()
}

fn normalize_workers(
    workers: Vec<String>,
    route: TaskRouteKind,
    profile: DomainProfile,
    message: &str,
) -> Vec<String> {
    let mut out = Vec::new();
    for worker in workers {
        if ALL_WORKER_ROLES
            .iter()
            .any(|allowed| allowed == &worker.as_str())
            && !out.contains(&worker)
        {
            out.push(worker);
        }
    }
    if out.is_empty() {
        select_workers(route, profile, message)
    } else {
        out
    }
}

fn select_workers(route: TaskRouteKind, profile: DomainProfile, message: &str) -> Vec<String> {
    match route {
        TaskRouteKind::ParallelReadonlyDelegation => select_readonly_workers(profile, message),
        TaskRouteKind::PlanOnly | TaskRouteKind::ComplexRuntime => {
            let mut workers = select_readonly_workers(profile, message);
            if workers.len() > 4 {
                workers.truncate(4);
            }
            workers
        }
        _ => Vec::new(),
    }
}

fn select_readonly_workers(profile: DomainProfile, message: &str) -> Vec<String> {
    let mut out = Vec::new();
    for area in matched_capability_areas(profile, message) {
        push_workers(&mut out, workers_for_area(area));
    }
    if out.is_empty() {
        push_workers(
            &mut out,
            &["project_explorer", "literature_scout", "data_profiler"],
        );
    }
    if out.len() > 1 {
        push_workers(&mut out, &["summary_writer"]);
    }
    out
}

fn workers_for_area(area: CapabilityArea) -> &'static [&'static str] {
    match area {
        CapabilityArea::Coding => &["project_explorer", "code_reviewer", "test_planner"],
        CapabilityArea::Data => &[
            "data_profiler",
            "analysis_reviewer",
            "reproducibility_planner",
        ],
        CapabilityArea::Academic => &["literature_scout", "evidence_reviewer", "synthesis_planner"],
        CapabilityArea::Medical => &[
            "medical_literature_scout",
            "clinical_evidence_reviewer",
            "safety_reviewer",
        ],
    }
}

fn push_workers(out: &mut Vec<String>, workers: &[&str]) {
    for worker in workers {
        if !out.iter().any(|existing| existing == worker) {
            out.push((*worker).to_string());
        }
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
    fn deterministic_router_blends_medical_data_and_code_workers() {
        let decision = route_deterministically(
            "分析这个医学 cohort 数据集和 Python notebook，审查临床证据和复现代码",
        );
        assert_eq!(decision.route, TaskRouteKind::ParallelReadonlyDelegation);
        for expected in [
            "medical_literature_scout",
            "clinical_evidence_reviewer",
            "data_profiler",
            "analysis_reviewer",
            "code_reviewer",
            "test_planner",
            "summary_writer",
        ] {
            assert!(
                decision.suggested_workers.iter().any(|w| w == expected),
                "expected blended worker {expected}, got {:?}",
                decision.suggested_workers
            );
        }
    }

    #[test]
    fn deterministic_router_blends_academic_and_data_workers() {
        let decision = route_deterministically("review 这些论文里的实验数据、统计指标和图表证据");
        assert_eq!(decision.route, TaskRouteKind::ParallelReadonlyDelegation);
        for expected in ["literature_scout", "evidence_reviewer", "data_profiler"] {
            assert!(
                decision.suggested_workers.iter().any(|w| w == expected),
                "expected blended worker {expected}, got {:?}",
                decision.suggested_workers
            );
        }
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

    #[tokio::test]
    async fn task_mode_forces_complex_runtime_not_plan_only() {
        let decision = route_message(None, "帮我处理这个任务", InteractionMode::Task).await;
        assert_eq!(decision.route, TaskRouteKind::ComplexRuntime);
        assert_eq!(decision.reason, "forced task mode");
    }

    #[tokio::test]
    async fn task_mode_forces_parallel_delegation_for_readonly_analysis() {
        let decision = route_message(None, "请分析这个项目架构", InteractionMode::Task).await;
        assert_eq!(decision.route, TaskRouteKind::ParallelReadonlyDelegation);
        assert!(decision.route.should_auto_execute());
    }

    #[test]
    fn deterministic_readonly_signal_upgrades_llm_chat_route() {
        let llm = TaskRouteDecision::normal("llm chose chat");
        let deterministic = route_deterministically("请分析这个项目架构");
        let decision = reconcile_llm_with_deterministic(llm, deterministic);
        assert_eq!(decision.route, TaskRouteKind::ParallelReadonlyDelegation);
        assert!(decision.reason.contains("read-only fanout override"));
    }
}
