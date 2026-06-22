//! Task router for choosing chat vs first-class runtime orchestration.
//!
//! This is intentionally above the old complexity classifier: the classifier is
//! a cheap fallback signal, while the router decides the product path.

use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;

use chrono::Utc;
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
    #[serde(default)]
    pub matched_feedback_pattern: Option<String>,
}

impl TaskRouteDecision {
    pub fn normal(reason: impl Into<String>) -> Self {
        Self {
            route: TaskRouteKind::NormalChat,
            confidence: 1.0,
            reason: reason.into(),
            suggested_workers: Vec::new(),
            classification: HeuristicClassifier::new().classify(""),
            matched_feedback_pattern: None,
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
    /// Number of Auto-route decisions corrected by this rule.
    #[serde(default)]
    pub hit_count: u64,
    /// Last UTC timestamp when this rule matched.
    #[serde(default)]
    pub last_matched_at: Option<String>,
}

/// User feedback action on a route decision.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RouteFeedbackAction {
    Correct,
    ShouldBeChat,
    ShouldBeTask,
    ShouldBeReadonlyParallel,
    TooManyWorkers,
    TooFewWorkers,
}

/// Persistent record of every route decision, including user feedback.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouteDecisionRecord {
    /// SHA-256 hex of the user message (first 16 chars).
    pub message_hash: String,
    /// Original user message for learning attribution. This stays local in the
    /// user's route history and lets feedback match future natural-language
    /// requests instead of trying to learn from an opaque hash.
    #[serde(default)]
    pub message_text: Option<String>,
    /// The route that was selected.
    pub route: TaskRouteKind,
    /// Confidence from the router at decision time.
    pub confidence: f32,
    /// Feedback pattern that matched (if any).
    pub matched_feedback_pattern: Option<String>,
    /// Workers suggested by the router.
    pub suggested_workers: Vec<String>,
    /// Workers actually used (None if no run was created).
    pub actual_workers: Option<Vec<String>>,
    /// Final status of the run, if a run was created.
    pub final_run_status: Option<String>,
    /// User correction submitted after the decision.
    pub user_correction: Option<RouteFeedbackAction>,
    /// When this decision was made.
    pub created_at: String,
}

/// Scored route feedback rule — extends RouteFeedbackRule with learning stats.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScoredRouteFeedbackRule {
    #[serde(flatten)]
    pub rule: RouteFeedbackRule,
    #[serde(default)]
    pub success_count: u64,
    #[serde(default)]
    pub failure_count: u64,
    /// Running score 0.0–1.0: success / (success + failure).
    /// Returns 0.5 when no feedback yet (neutral).
    #[serde(default = "default_score")]
    pub score: f64,
    #[serde(default)]
    pub last_failure_reason: Option<String>,
}

fn default_score() -> f64 {
    0.5
}

// ── Route decision record persistence (JSONL, append-only) ────────────

pub fn default_route_records_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home)
        .join(".echo-agent")
        .join("route_records.jsonl")
}

pub fn load_route_records() -> Vec<RouteDecisionRecord> {
    let path = default_route_records_path();
    match std::fs::read_to_string(&path) {
        Ok(raw) => raw
            .lines()
            .filter(|l| !l.trim().is_empty())
            .filter_map(|l| {
                serde_json::from_str(l)
                    .map_err(
                        |e| tracing::warn!(error = %e, line = %l, "skipping corrupt route record"),
                    )
                    .ok()
            })
            .collect(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => {
            tracing::warn!(error = %e, "failed to read route records");
            Vec::new()
        }
    }
}

pub fn append_route_record(record: &RouteDecisionRecord) -> anyhow::Result<()> {
    let path = default_route_records_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut line = serde_json::to_string(record)?;
    line.push('\n');
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?
        .write_all(line.as_bytes())?;
    Ok(())
}

pub fn compute_scored_rules(
    rules: &[RouteFeedbackRule],
    records: &[RouteDecisionRecord],
) -> Vec<ScoredRouteFeedbackRule> {
    rules
        .iter()
        .map(|rule| {
            let mut success = 0u64;
            let mut failure = 0u64;
            let mut last_reason = None;
            for record in records {
                if record
                    .matched_feedback_pattern
                    .as_deref()
                    .is_some_and(|p| p == rule.pattern)
                {
                    match record.user_correction {
                        Some(RouteFeedbackAction::Correct) => success += 1,
                        Some(action) => {
                            failure += 1;
                            last_reason = Some(format!("user corrected to {:?}", action));
                        }
                        None => {} // no feedback yet
                    }
                }
            }
            let total = success + failure;
            let score = if total == 0 {
                0.5 // neutral: no feedback yet
            } else {
                success as f64 / total as f64
            };
            ScoredRouteFeedbackRule {
                rule: rule.clone(),
                success_count: success,
                failure_count: failure,
                score,
                last_failure_reason: last_reason,
            }
        })
        .collect()
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

pub fn record_route_feedback_match(message: &str, rules: &mut [RouteFeedbackRule]) -> bool {
    let Some(rule) = rules
        .iter_mut()
        .find(|rule| route_feedback_matches(message, &rule.pattern))
    else {
        return false;
    };

    update_route_feedback_hit(rule);
    true
}

pub fn record_route_feedback_pattern(pattern: &str, rules: &mut [RouteFeedbackRule]) -> bool {
    let key = normalize_feedback_text(pattern);
    let Some(rule) = rules
        .iter_mut()
        .find(|rule| normalize_feedback_text(&rule.pattern) == key)
    else {
        return false;
    };

    update_route_feedback_hit(rule);
    true
}

fn update_route_feedback_hit(rule: &mut RouteFeedbackRule) {
    rule.hit_count = rule.hit_count.saturating_add(1);
    rule.last_matched_at = Some(Utc::now().to_rfc3339());
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
    #[serde(default)]
    matched_feedback_pattern: Option<String>,
}

/// Route with an LLM first, falling back to deterministic local signals.
pub async fn route_message(
    llm: Option<Arc<dyn LlmClient>>,
    message: &str,
    mode: InteractionMode,
    cache_user_id: Option<&str>,
) -> TaskRouteDecision {
    route_message_with_feedback(llm, message, mode, &[], cache_user_id).await
}

/// Route a message and apply historical feedback corrections in Auto mode.
pub async fn route_message_with_feedback(
    llm: Option<Arc<dyn LlmClient>>,
    message: &str,
    mode: InteractionMode,
    feedback_rules: &[RouteFeedbackRule],
    cache_user_id: Option<&str>,
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
        && let Ok(decision) = route_with_llm(&llm, message, feedback_rules, cache_user_id).await
    {
        reconcile_llm_with_deterministic(decision, deterministic)
    } else {
        deterministic
    };

    // Compute scored rules from feedback history for auto-correction
    let records = load_route_records();
    let scored = compute_scored_rules(feedback_rules, &records);
    apply_route_feedback(decision, message, feedback_rules, &scored)
}

fn forced_decision(route: TaskRouteKind, message: &str, reason: &str) -> TaskRouteDecision {
    let classification = HeuristicClassifier::new().classify(message);
    TaskRouteDecision {
        route,
        confidence: 1.0,
        reason: reason.to_string(),
        suggested_workers: select_workers(route, classification.inferred_profile, message),
        classification,
        matched_feedback_pattern: None,
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
        matched_feedback_pattern: None,
    }
}

fn reconcile_llm_with_deterministic(
    mut llm: TaskRouteDecision,
    deterministic: TaskRouteDecision,
) -> TaskRouteDecision {
    if llm.matched_feedback_pattern.is_some() {
        return llm;
    }
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
    scored_rules: &[ScoredRouteFeedbackRule],
) -> TaskRouteDecision {
    let Some(rule) = feedback_rules
        .iter()
        .find(|rule| route_feedback_matches(message, &rule.pattern))
    else {
        return decision;
    };

    // ── Auto-correction based on scoring ──────────────────────────────
    if let Some(scored) = scored_rules.iter().find(|s| s.rule.pattern == rule.pattern) {
        // Low-score rules: skip the override entirely
        if scored.score < 0.3 && scored.failure_count >= 3 {
            tracing::info!(
                pattern = %rule.pattern,
                score = %scored.score,
                failures = scored.failure_count,
                "skipping low-score route feedback override — rule has been wrong repeatedly"
            );
            return decision;
        }
        // High-score rules: apply with max confidence
        if scored.score > 0.9 && scored.success_count >= 5 {
            decision.confidence = 1.0;
        }
    }

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
    decision.matched_feedback_pattern = Some(rule.pattern.clone());
    if matches!(
        rule.route,
        TaskRouteKind::NormalChat | TaskRouteKind::DirectEdit | TaskRouteKind::BackgroundTask
    ) {
        decision.suggested_workers.clear();
    }
    decision
}

pub fn route_feedback_matches(message: &str, pattern: &str) -> bool {
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

fn find_route_feedback_rule<'a>(
    feedback_rules: &'a [RouteFeedbackRule],
    pattern: &str,
) -> Option<&'a RouteFeedbackRule> {
    let key = normalize_feedback_text(pattern);
    feedback_rules
        .iter()
        .find(|rule| normalize_feedback_text(&rule.pattern) == key)
}

fn route_feedback_prompt(feedback_rules: &[RouteFeedbackRule]) -> String {
    if feedback_rules.is_empty() {
        return "Historical route feedback: []".to_string();
    }

    let mut rules = feedback_rules.to_vec();
    rules.sort_by(|a, b| {
        b.hit_count
            .cmp(&a.hit_count)
            .then_with(|| a.pattern.cmp(&b.pattern))
    });

    let lines = rules
        .iter()
        .take(12)
        .map(|rule| {
            format!(
                r#"{{"pattern":"{}","route":"{}","reason":"{}","workers":[{}],"hits":{}}}"#,
                feedback_prompt_text(&rule.pattern, 120),
                rule.route.as_str(),
                feedback_prompt_text(&rule.reason, 160),
                rule.suggested_workers
                    .iter()
                    .take(6)
                    .map(|worker| format!(r#""{}""#, feedback_prompt_text(worker, 80)))
                    .collect::<Vec<_>>()
                    .join(","),
                rule.hit_count,
            )
        })
        .collect::<Vec<_>>()
        .join("\n");

    format!(
        "Historical route feedback rules. If the user message is semantically equivalent to one rule, set matched_feedback_pattern exactly to that rule's pattern and choose that route. Do not invent patterns.\n{lines}"
    )
}

fn feedback_prompt_text(value: &str, max_chars: usize) -> String {
    value
        .chars()
        .take(max_chars)
        .collect::<String>()
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', " ")
}

async fn route_with_llm(
    llm: &Arc<dyn LlmClient>,
    message: &str,
    feedback_rules: &[RouteFeedbackRule],
    cache_user_id: Option<&str>,
) -> Result<TaskRouteDecision, String> {
    let request = ChatRequest {
        messages: vec![
            Message::system(router_system_prompt()),
            Message::user(format!(
                "{}\n\nUser message:\n{message}",
                route_feedback_prompt(feedback_rules)
            )),
        ],
        response_format: Some(ResponseFormat::JsonObject),
        user_id: cache_user_id.map(|s| s.to_string()),
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
    let semantic_feedback = raw
        .matched_feedback_pattern
        .as_deref()
        .and_then(|pattern| find_route_feedback_rule(feedback_rules, pattern));
    if let Some(rule) = semantic_feedback {
        return Ok(TaskRouteDecision {
            route: rule.route,
            confidence: confidence.max(0.9),
            reason: format!(
                "semantic router feedback override: {}; llm_reason={}",
                if rule.reason.trim().is_empty() {
                    "matched prior user correction"
                } else {
                    rule.reason.trim()
                },
                raw.reason
                    .filter(|s| !s.trim().is_empty())
                    .unwrap_or_else(|| "LLM matched historical feedback".to_string())
            ),
            suggested_workers: normalize_workers(
                rule.suggested_workers.clone(),
                rule.route,
                classification.inferred_profile,
                message,
            ),
            classification,
            matched_feedback_pattern: Some(rule.pattern.clone()),
        });
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
        matched_feedback_pattern: None,
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
  "suggested_workers": string[],
  "matched_feedback_pattern": string | null
}}

EKO's main domains are AI coding, academic research, data processing/analysis, and medical research. Choose "parallel_readonly_delegation" for broad read-only work that benefits from multiple workers in any of these domains: project architecture analysis, codebase review, literature exploration, evidence review, dataset profiling, analysis-method review, clinical evidence review, safety review, or comparing several candidate root causes.

Domains are context labels, not worker boundaries. A medical or academic task may need data-analysis workers; a data task may need coding workers; a coding task may need literature/evidence workers. Suggested workers are not a fixed count. Pick the smallest useful cross-domain set from this capability catalog:
{catalog}

Choose "plan_only" when the user explicitly asks for a plan or says not to execute.
Choose "complex_runtime" for multi-step work that may include edits, verification, or review gates.
Choose "normal_chat" for small questions, explanations, or single-file/simple answers.
Choose "direct_edit" only for tiny obvious edits.
Choose "background_task" for long detached tasks the user expects to run asynchronously.

If the user message is semantically equivalent to a historical route feedback rule provided in the user message, set matched_feedback_pattern to that rule's exact pattern. Otherwise set it to null."#,
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
        matched_feedback_pattern: None,
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

    fn feedback_rule(
        pattern: &str,
        route: TaskRouteKind,
        reason: &str,
        suggested_workers: Vec<String>,
    ) -> RouteFeedbackRule {
        RouteFeedbackRule {
            pattern: pattern.to_string(),
            route,
            reason: reason.to_string(),
            suggested_workers,
            hit_count: 0,
            last_matched_at: None,
        }
    }

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
        let decision = route_message(None, "帮我分析当前目录的项目", InteractionMode::Auto, None).await;
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
        let feedback = vec![feedback_rule(
            "帮我分析当前目录的项目",
            TaskRouteKind::NormalChat,
            "user rejected TaskRuntime for this prompt",
            Vec::new(),
        )];
        let decision = route_message_with_feedback(
            None,
            "帮我分析当前目录的项目",
            InteractionMode::Auto,
            &feedback,
            None,
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
        let feedback = vec![feedback_rule(
            "closure 是什么",
            TaskRouteKind::ParallelReadonlyDelegation,
            "user asked to always inspect the codebase for this phrase",
            vec![
                "project_explorer".to_string(),
                "not_a_worker".to_string(),
                "code_reviewer".to_string(),
            ],
        )];
        let decision = route_message_with_feedback(
            None,
            "Rust 的 closure 是什么？",
            InteractionMode::Auto,
            &feedback,
            None,
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
        let feedback = vec![feedback_rule(
            "帮我处理这个任务",
            TaskRouteKind::NormalChat,
            "prior correction",
            Vec::new(),
        )];
        let decision =
            route_message_with_feedback(None, "帮我处理这个任务", InteractionMode::Task, &feedback, None)
                .await;
        assert_eq!(decision.route, TaskRouteKind::ComplexRuntime);
        assert!(decision.reason.contains("forced task mode"));
    }

    #[test]
    fn record_route_feedback_match_updates_hit_stats() {
        let mut feedback = vec![feedback_rule(
            "分析当前目录",
            TaskRouteKind::ParallelReadonlyDelegation,
            "prior correction",
            Vec::new(),
        )];
        assert!(record_route_feedback_match(
            "请帮我分析当前目录的项目",
            &mut feedback
        ));
        assert_eq!(feedback.len(), 1);
        if let Some(rule) = feedback.first() {
            assert_eq!(rule.hit_count, 1);
            assert!(rule.last_matched_at.is_some());
        }
    }

    #[test]
    fn record_route_feedback_pattern_updates_semantic_hit_stats() {
        let mut feedback = vec![feedback_rule(
            "看下这个仓库架构",
            TaskRouteKind::ParallelReadonlyDelegation,
            "semantic prior correction",
            Vec::new(),
        )];
        assert!(record_route_feedback_pattern(
            "看下这个仓库架构",
            &mut feedback
        ));
        assert_eq!(feedback.len(), 1);
        if let Some(rule) = feedback.first() {
            assert_eq!(rule.hit_count, 1);
            assert!(rule.last_matched_at.is_some());
        }
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
        let decision = route_message(None, "帮我处理这个任务", InteractionMode::Task, None).await;
        assert_eq!(decision.route, TaskRouteKind::ComplexRuntime);
        assert!(decision.reason.contains("forced task mode"));
        assert!(decision.reason.contains("routing_signals"));
    }

    #[tokio::test]
    async fn task_mode_forces_parallel_delegation_for_readonly_analysis() {
        let decision = route_message(None, "请分析这个项目架构", InteractionMode::Task, None).await;
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

    #[test]
    fn semantic_feedback_route_is_not_overridden_by_deterministic_readonly() {
        let mut llm = TaskRouteDecision::normal("semantic router feedback override");
        llm.matched_feedback_pattern = Some("看下这个仓库架构".to_string());
        let deterministic = route_deterministically("请分析这个项目架构");
        let decision = reconcile_llm_with_deterministic(llm, deterministic);
        assert_eq!(decision.route, TaskRouteKind::NormalChat);
        assert_eq!(
            decision.matched_feedback_pattern,
            Some("看下这个仓库架构".to_string())
        );
    }
}
