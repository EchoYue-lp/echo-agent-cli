//! Task router for choosing chat vs first-class runtime orchestration.
//!
//! This is intentionally above the old complexity classifier: the classifier is
//! a cheap fallback signal, while the router decides the product path.

use std::path::PathBuf;
use std::sync::Arc;

use echo_agent::llm::{ChatRequest, LlmClient, ResponseFormat};
use echo_agent::prelude::Message;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::classify::{Classification, ComplexityLabel, HeuristicClassifier};
use super::delegation::{
    DEFAULT_MAX_READONLY_WORKERS, DelegationPlanner, DelegationRequest, worker_role_names,
};
use super::profiles::{ALL_WORKER_ROLES, worker_catalog_prompt};
use super::signals::RoutingSignals;
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

/// User or runtime correction learned from prior route decisions.
///
/// The router itself is LLM-first, with deterministic signals as the safety
/// net. Feedback rules are the third layer: they let the product remember
/// "messages like this should be handled as that route" without expanding the
/// heuristic cue tables forever.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouteFeedbackRule {
    /// A normalized phrase or full user request to match against.
    pub pattern: String,
    /// Route to force when the pattern matches in Auto mode.
    pub route: TaskRouteKind,
    /// Human-readable correction reason shown in route traces.
    pub reason: String,
    /// Optional worker override. Invalid worker names are ignored.
    #[serde(default)]
    pub suggested_workers: Vec<String>,
}

pub fn default_route_feedback_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home)
        .join(".echo-agent")
        .join("route_feedback.json")
}

pub fn load_route_feedback_rules() -> Vec<RouteFeedbackRule> {
    match try_load_route_feedback_rules() {
        Ok(rules) => rules,
        Err(error) => {
            tracing::warn!(%error, "failed to load route feedback rules");
            Vec::new()
        }
    }
}

pub fn try_load_route_feedback_rules() -> anyhow::Result<Vec<RouteFeedbackRule>> {
    let path = default_route_feedback_path();
    match std::fs::read_to_string(&path) {
        Ok(raw) => Ok(serde_json::from_str(&raw)?),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(error.into()),
    }
}

pub fn save_route_feedback_rules(rules: &[RouteFeedbackRule]) -> anyhow::Result<()> {
    let path = default_route_feedback_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let raw = serde_json::to_string_pretty(rules)?;
    std::fs::write(path, raw)?;
    Ok(())
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
    route_message_with_feedback(llm, message, mode, &[]).await
}

/// Route a message and apply historical feedback corrections in Auto mode.
pub async fn route_message_with_feedback(
    llm: Option<Arc<dyn LlmClient>>,
    message: &str,
    mode: InteractionMode,
    feedback_rules: &[RouteFeedbackRule],
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
    let decision = if let Some(llm) = llm
        && let Ok(decision) = route_with_llm(&llm, message).await
    {
        reconcile_llm_with_deterministic(decision, deterministic)
    } else {
        deterministic
    };

    apply_route_feedback(decision, message, feedback_rules)
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
    let classification = HeuristicClassifier::new().classify(message);
    let signals = RoutingSignals::analyze(classification.inferred_profile, message);
    let route = if signals.supports_parallel_readonly() {
        TaskRouteKind::ParallelReadonlyDelegation
    } else {
        TaskRouteKind::ComplexRuntime
    };
    TaskRouteDecision {
        route,
        confidence: 1.0,
        reason: format!("forced task mode; {}", signals.reason_suffix()),
        suggested_workers: select_workers_from_signals(route, &signals),
        classification,
    }
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

fn apply_route_feedback(
    mut decision: TaskRouteDecision,
    message: &str,
    feedback_rules: &[RouteFeedbackRule],
) -> TaskRouteDecision {
    let Some(rule) = feedback_rules
        .iter()
        .find(|rule| route_feedback_matches(message, &rule.pattern))
    else {
        return decision;
    };

    let previous_route = decision.route.as_str();
    decision.route = rule.route;
    decision.confidence = decision.confidence.max(0.95);
    decision.reason = format!(
        "historical router feedback override: {}; previous_route={previous_route}",
        if rule.reason.trim().is_empty() {
            "matched prior user correction"
        } else {
            rule.reason.trim()
        }
    );
    decision.suggested_workers = normalize_workers(
        rule.suggested_workers.clone(),
        rule.route,
        decision.classification.inferred_profile,
        message,
    );
    if matches!(
        rule.route,
        TaskRouteKind::NormalChat | TaskRouteKind::DirectEdit | TaskRouteKind::BackgroundTask
    ) {
        decision.suggested_workers.clear();
    }
    decision
}

fn route_feedback_matches(message: &str, pattern: &str) -> bool {
    let normalized_message = normalize_feedback_text(message);
    let normalized_pattern = normalize_feedback_text(pattern);
    !normalized_pattern.is_empty()
        && (normalized_message == normalized_pattern
            || normalized_message.contains(&normalized_pattern))
}

fn normalize_feedback_text(text: &str) -> String {
    text.to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
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
    let signals = RoutingSignals::analyze(classification.inferred_profile, message);
    let route = if signals.plan_only {
        TaskRouteKind::PlanOnly
    } else if signals.supports_parallel_readonly() {
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
        reason: format!(
            "deterministic fallback: {}; {}",
            classification.reason,
            signals.reason_suffix()
        ),
        suggested_workers: select_workers_from_signals(route, &signals),
        classification,
    }
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
    let signals = RoutingSignals::analyze(profile, message);
    select_workers_from_signals(route, &signals)
}

fn select_workers_from_signals(route: TaskRouteKind, signals: &RoutingSignals) -> Vec<String> {
    match route {
        TaskRouteKind::ParallelReadonlyDelegation => select_readonly_workers(signals),
        TaskRouteKind::PlanOnly | TaskRouteKind::ComplexRuntime => {
            let mut workers = select_readonly_workers(signals);
            if workers.len() > 4 {
                workers.truncate(4);
            }
            workers
        }
        _ => Vec::new(),
    }
}

fn select_readonly_workers(signals: &RoutingSignals) -> Vec<String> {
    worker_role_names(&DelegationPlanner::plan_readonly(DelegationRequest {
        goal: "",
        profile: DomainProfile::General,
        signals,
        suggested_workers: &[],
        max_workers: DEFAULT_MAX_READONLY_WORKERS,
    }))
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

    #[tokio::test]
    async fn auto_mode_routes_current_directory_project_analysis_to_parallel_runtime() {
        let decision = route_message(None, "帮我分析当前目录的项目", InteractionMode::Auto).await;
        assert_eq!(decision.route, TaskRouteKind::ParallelReadonlyDelegation);
        assert!(decision.route.should_auto_execute());
        assert!(
            decision
                .suggested_workers
                .iter()
                .any(|w| w == "project_explorer"),
            "expected project_explorer worker, got {:?}",
            decision.suggested_workers
        );
    }

    #[tokio::test]
    async fn auto_mode_applies_feedback_to_keep_repeated_prompt_in_chat() {
        let feedback = vec![RouteFeedbackRule {
            pattern: "帮我分析当前目录的项目".to_string(),
            route: TaskRouteKind::NormalChat,
            reason: "user rejected TaskRuntime for this prompt".to_string(),
            suggested_workers: Vec::new(),
        }];
        let decision = route_message_with_feedback(
            None,
            "帮我分析当前目录的项目",
            InteractionMode::Auto,
            &feedback,
        )
        .await;
        assert_eq!(decision.route, TaskRouteKind::NormalChat);
        assert!(decision.suggested_workers.is_empty());
        assert!(decision.reason.contains("historical router feedback"));
        assert!(
            decision
                .reason
                .contains("user rejected TaskRuntime for this prompt")
        );
    }

    #[tokio::test]
    async fn auto_mode_feedback_can_force_parallel_workers() {
        let feedback = vec![RouteFeedbackRule {
            pattern: "closure 是什么".to_string(),
            route: TaskRouteKind::ParallelReadonlyDelegation,
            reason: "user asked to always inspect the codebase for this phrase".to_string(),
            suggested_workers: vec![
                "project_explorer".to_string(),
                "not_a_worker".to_string(),
                "code_reviewer".to_string(),
            ],
        }];
        let decision = route_message_with_feedback(
            None,
            "Rust 的 closure 是什么？",
            InteractionMode::Auto,
            &feedback,
        )
        .await;
        assert_eq!(decision.route, TaskRouteKind::ParallelReadonlyDelegation);
        assert_eq!(
            decision.suggested_workers,
            vec!["project_explorer".to_string(), "code_reviewer".to_string()]
        );
        assert!(decision.confidence >= 0.95);
    }

    #[tokio::test]
    async fn forced_modes_ignore_feedback() {
        let feedback = vec![RouteFeedbackRule {
            pattern: "帮我处理这个任务".to_string(),
            route: TaskRouteKind::NormalChat,
            reason: "prior correction".to_string(),
            suggested_workers: Vec::new(),
        }];
        let decision =
            route_message_with_feedback(None, "帮我处理这个任务", InteractionMode::Task, &feedback)
                .await;
        assert_eq!(decision.route, TaskRouteKind::ComplexRuntime);
        assert!(decision.reason.contains("forced task mode"));
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
        assert!(decision.reason.contains("forced task mode"));
        assert!(decision.reason.contains("routing_signals"));
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
