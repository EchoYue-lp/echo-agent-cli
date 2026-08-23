use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use echo_agent::trace::{Run, RunEvent, RunStatus, RunStore, RunSummary};

use crate::project::prompt::PromptAssembly;

use super::types::{
    CacheDiagnostic, CompressionDiagnostic, ContextDiagnostic, DiagnosticIssue,
    DiagnosticRunSummary, DiagnosticSeverity, LlmCallDiagnostic, RunDiagnostics,
    RunUsageDiagnostic, TraceInvocationDiagnostic, UsageSource,
};

const MAX_LISTED_TRACES: usize = 500;
const PROTECTED_ABSOLUTE_WARNING_TOKENS: usize = 32_000;

pub async fn list_diagnostic_runs(
    store: &dyn RunStore,
) -> echo_agent::error::Result<Vec<DiagnosticRunSummary>> {
    let summaries = store.list_all(MAX_LISTED_TRACES).await?;
    let mut groups: BTreeMap<String, Vec<RunSummary>> = BTreeMap::new();
    for summary in summaries {
        let key = summary
            .parent_run_id
            .clone()
            .unwrap_or_else(|| summary.run_id.clone());
        groups.entry(key).or_default().push(summary);
    }

    let mut diagnostics = groups
        .into_iter()
        .filter_map(|(diagnostic_id, traces)| summarize_group(diagnostic_id, traces))
        .collect::<Vec<_>>();
    diagnostics.sort_by_key(|summary| std::cmp::Reverse(summary.started_at));
    Ok(diagnostics)
}

pub async fn load_run_diagnostics(
    store: &dyn RunStore,
    diagnostic_id: &str,
    prompt_assembly: Option<PromptAssembly>,
) -> echo_agent::error::Result<Option<RunDiagnostics>> {
    let child_summaries = store.list_by_parent_run(diagnostic_id).await?;
    let mut runs = Vec::new();
    for summary in child_summaries {
        if let Some(run) = store.load(summary.run_id.as_str()).await? {
            runs.push(run);
        }
    }
    if runs.is_empty()
        && let Some(run) = store.load(diagnostic_id).await?
    {
        runs.push(run);
    }
    if runs.is_empty() {
        return Ok(None);
    }
    runs.sort_by_key(|run| run.started_at);
    Ok(Some(build_run_diagnostics(
        diagnostic_id,
        runs,
        prompt_assembly,
    )))
}

pub fn format_run_diagnostics(diagnostics: &RunDiagnostics) -> String {
    let mut output = String::new();
    output.push_str(&format!("Run diagnostics: {}\n", diagnostics.diagnostic_id));
    output.push_str(&format!(
        "  Traces: {} | provider usage calls: {} | missing usage: {}\n",
        diagnostics.traces.len(),
        diagnostics.usage.provider_reported_calls,
        diagnostics.usage.calls_missing_usage,
    ));
    output.push_str(&format!(
        "  Provider totals: input {} | output {} | cached {} | cache write {}\n",
        diagnostics.usage.total_input_tokens,
        diagnostics.usage.total_output_tokens,
        diagnostics.usage.total_cached_input_tokens,
        diagnostics.usage.total_cache_creation_input_tokens,
    ));
    let cache_rate = diagnostics
        .cache
        .read_rate
        .map(|rate| format!("{:.1}%", rate * 100.0))
        .unwrap_or_else(|| "unknown".to_string());
    output.push_str(&format!(
        "  Cache read: {} | system changes {} | tools changes {}\n",
        cache_rate,
        diagnostics.cache.system_prefix_hash_changes,
        diagnostics.cache.tools_schema_hash_changes,
    ));
    output.push_str(&format!(
        "  Latest context: provider {:?} | estimated ~{} / {} | protected max ~{} ({} messages)\n",
        diagnostics.context.latest_provider_input_tokens,
        diagnostics.context.latest_estimated_context_tokens,
        diagnostics.context.latest_context_limit_tokens,
        diagnostics.context.max_protected_context_tokens,
        diagnostics.context.max_protected_message_count,
    ));
    if !diagnostics.compressions.is_empty() {
        output.push_str("  Compressions:\n");
        for compression in &diagnostics.compressions {
            output.push_str(&format!(
                "    {}: {} -> {} tokens ({} -> {} messages)\n",
                compression.source,
                compression.before_tokens,
                compression.after_tokens,
                compression.before_messages,
                compression.after_messages,
            ));
        }
    }
    if !diagnostics.issues.is_empty() {
        output.push_str("  Issues:\n");
        for issue in &diagnostics.issues {
            output.push_str(&format!("    {:?}: {}\n", issue.severity, issue.message));
        }
    }
    output
}

fn summarize_group(diagnostic_id: String, traces: Vec<RunSummary>) -> Option<DiagnosticRunSummary> {
    let first = traces.iter().min_by_key(|trace| trace.started_at)?;
    let mut agents = BTreeSet::new();
    let mut models = BTreeSet::new();
    let mut total_input = 0u64;
    let mut total_output = 0u64;
    let mut total_cached = 0u64;
    let mut llm_calls = 0usize;
    let mut missing_usage = 0usize;
    let mut finished_at = None;
    for trace in &traces {
        if !trace.agent_name.is_empty() {
            agents.insert(trace.agent_name.clone());
        }
        if !trace.model.is_empty() {
            models.insert(trace.model.clone());
        }
        total_input = total_input.saturating_add(u64::from(trace.token_usage.prompt_tokens));
        total_output = total_output.saturating_add(u64::from(trace.token_usage.completion_tokens));
        total_cached =
            total_cached.saturating_add(u64::from(trace.token_usage.cached_prompt_tokens));
        llm_calls = llm_calls.saturating_add(
            trace.token_usage.usage_reported_calls as usize
                + trace.token_usage.usage_missing_calls as usize,
        );
        missing_usage =
            missing_usage.saturating_add(trace.token_usage.usage_missing_calls as usize);
        if let Some(value) = trace.finished_at
            && finished_at.is_none_or(|current| value > current)
        {
            finished_at = Some(value);
        }
    }
    let status = aggregate_status(&traces);
    Some(DiagnosticRunSummary {
        parent_run_id: traces.first().and_then(|trace| trace.parent_run_id.clone()),
        diagnostic_id,
        trace_count: traces.len(),
        status,
        input_preview: first.input_preview.clone(),
        started_at: first.started_at,
        finished_at,
        agents: agents.into_iter().collect(),
        models: models.into_iter().collect(),
        total_input_tokens: total_input,
        total_output_tokens: total_output,
        total_cached_input_tokens: total_cached,
        llm_calls,
        calls_missing_usage: missing_usage,
    })
}

fn aggregate_status(traces: &[RunSummary]) -> String {
    if traces
        .iter()
        .any(|trace| trace.status == RunStatus::Running)
    {
        return "running".to_string();
    }
    if traces.iter().any(|trace| trace.status == RunStatus::Failed) {
        return "failed".to_string();
    }
    if traces
        .iter()
        .any(|trace| trace.status == RunStatus::Cancelled)
    {
        return "cancelled".to_string();
    }
    if traces
        .iter()
        .all(|trace| trace.status == RunStatus::Completed)
    {
        return "completed".to_string();
    }
    "pending".to_string()
}

fn build_run_diagnostics(
    diagnostic_id: &str,
    runs: Vec<Run>,
    prompt_assembly: Option<PromptAssembly>,
) -> RunDiagnostics {
    let parent_run_id = runs.iter().find_map(|run| run.parent_run_id.clone());
    let mut usage = RunUsageDiagnostic::default();
    let mut context = ContextDiagnostic::default();
    let mut compressions = Vec::new();
    let mut trace_diagnostics = Vec::new();
    let mut component_hashes: HashMap<String, ComponentHashes> = HashMap::new();

    for run in runs {
        let component_key = format!("{}:{}", run.agent_name, run.model);
        let hashes = component_hashes.entry(component_key).or_default();
        let mut llm_calls = Vec::new();
        for (sequence, event) in run.events.iter().enumerate() {
            match event {
                RunEvent::LlmCall {
                    messages,
                    prompt_tokens,
                    completion_tokens,
                    cached_prompt_tokens,
                    cache_creation_prompt_tokens,
                    usage_reported,
                    estimated_context_tokens,
                    protected_context_tokens,
                    protected_message_count,
                    context_limit_tokens,
                    context_breakdown,
                    cache_fingerprint,
                    duration_ms,
                } => {
                    if *usage_reported {
                        usage.provider_reported_calls =
                            usage.provider_reported_calls.saturating_add(1);
                        usage.total_input_tokens = usage
                            .total_input_tokens
                            .saturating_add(u64::from(*prompt_tokens));
                        usage.total_output_tokens = usage
                            .total_output_tokens
                            .saturating_add(u64::from(*completion_tokens));
                        usage.total_cached_input_tokens = usage
                            .total_cached_input_tokens
                            .saturating_add(u64::from(*cached_prompt_tokens));
                        usage.total_cache_creation_input_tokens = usage
                            .total_cache_creation_input_tokens
                            .saturating_add(u64::from(*cache_creation_prompt_tokens));
                        context.latest_provider_input_tokens = Some(u64::from(*prompt_tokens));
                    } else {
                        usage.calls_missing_usage = usage.calls_missing_usage.saturating_add(1);
                    }
                    context.latest_estimated_context_tokens = *estimated_context_tokens;
                    context.latest_context_limit_tokens = *context_limit_tokens;
                    context.latest_breakdown = context_breakdown.clone();
                    context.max_protected_context_tokens = context
                        .max_protected_context_tokens
                        .max(*protected_context_tokens);
                    context.max_protected_message_count = context
                        .max_protected_message_count
                        .max(*protected_message_count);
                    hashes
                        .system
                        .insert(cache_fingerprint.system_prefix_hash.clone());
                    hashes
                        .tools
                        .insert(cache_fingerprint.tools_schema_hash.clone());
                    hashes
                        .stable
                        .insert(cache_fingerprint.stable_prefix_hash.clone());
                    llm_calls.push(LlmCallDiagnostic {
                        sequence,
                        source: if *usage_reported {
                            UsageSource::Provider
                        } else {
                            UsageSource::Estimated
                        },
                        input_tokens: (*usage_reported).then_some(u64::from(*prompt_tokens)),
                        output_tokens: (*usage_reported).then_some(u64::from(*completion_tokens)),
                        cached_input_tokens: (*usage_reported)
                            .then_some(u64::from(*cached_prompt_tokens)),
                        cache_creation_input_tokens: (*usage_reported)
                            .then_some(u64::from(*cache_creation_prompt_tokens)),
                        estimated_context_tokens: *estimated_context_tokens,
                        protected_context_tokens: *protected_context_tokens,
                        protected_message_count: *protected_message_count,
                        context_limit_tokens: *context_limit_tokens,
                        context_breakdown: context_breakdown.clone(),
                        stable_prefix_hash: cache_fingerprint.stable_prefix_hash.clone(),
                        system_prefix_hash: cache_fingerprint.system_prefix_hash.clone(),
                        tools_schema_hash: cache_fingerprint.tools_schema_hash.clone(),
                        history_hash: cache_fingerprint.history_hash.clone(),
                        message_count: *messages,
                        tool_count: cache_fingerprint.tool_count,
                        duration_ms: *duration_ms,
                    });
                }
                RunEvent::ContextCompression {
                    source,
                    before_messages,
                    after_messages,
                    before_tokens,
                    after_tokens,
                    protected_context_tokens,
                    protected_message_count,
                } => compressions.push(CompressionDiagnostic {
                    trace_run_id: run.run_id.clone(),
                    sequence,
                    source: source.clone(),
                    before_messages: *before_messages,
                    after_messages: *after_messages,
                    before_tokens: *before_tokens,
                    after_tokens: *after_tokens,
                    protected_context_tokens: *protected_context_tokens,
                    protected_message_count: *protected_message_count,
                }),
                _ => {}
            }
        }
        trace_diagnostics.push(TraceInvocationDiagnostic {
            trace_run_id: run.run_id,
            agent_name: run.agent_name,
            model: run.model,
            provider: run.provider,
            turn_id: run.turn_id,
            execution_id: run.execution_id,
            status: run_status_name(run.status).to_string(),
            started_at: run.started_at,
            finished_at: run.finished_at,
            llm_calls,
        });
    }

    let cache = build_cache_diagnostic(&usage, component_hashes.values());
    let issues = build_issues(&usage, &cache, &context);
    RunDiagnostics {
        diagnostic_id: diagnostic_id.to_string(),
        parent_run_id,
        traces: trace_diagnostics,
        usage,
        cache,
        context,
        compressions,
        issues,
        prompt_assembly,
    }
}

#[derive(Default)]
struct ComponentHashes {
    system: HashSet<String>,
    tools: HashSet<String>,
    stable: HashSet<String>,
}

fn build_cache_diagnostic<'a>(
    usage: &RunUsageDiagnostic,
    hashes: impl Iterator<Item = &'a ComponentHashes>,
) -> CacheDiagnostic {
    let mut diagnostic = CacheDiagnostic {
        read_rate: (usage.total_input_tokens > 0)
            .then(|| usage.total_cached_input_tokens as f64 / usage.total_input_tokens as f64),
        ..Default::default()
    };
    for hash in hashes {
        diagnostic.system_prefix_hash_changes = diagnostic
            .system_prefix_hash_changes
            .saturating_add(non_empty_change_count(&hash.system));
        diagnostic.tools_schema_hash_changes = diagnostic
            .tools_schema_hash_changes
            .saturating_add(non_empty_change_count(&hash.tools));
        diagnostic.stable_prefix_hash_changes = diagnostic
            .stable_prefix_hash_changes
            .saturating_add(non_empty_change_count(&hash.stable));
    }
    diagnostic
}

fn non_empty_change_count(values: &HashSet<String>) -> usize {
    values
        .iter()
        .filter(|value| !value.is_empty())
        .count()
        .saturating_sub(1)
}

fn build_issues(
    usage: &RunUsageDiagnostic,
    cache: &CacheDiagnostic,
    context: &ContextDiagnostic,
) -> Vec<DiagnosticIssue> {
    let mut issues = Vec::new();
    if usage.calls_missing_usage > 0 {
        issues.push(DiagnosticIssue {
            kind: "missing_provider_usage".to_string(),
            severity: DiagnosticSeverity::Warning,
            message: format!(
                "{} LLM calls omitted provider usage; exact totals exclude those calls",
                usage.calls_missing_usage
            ),
        });
    }
    if cache.system_prefix_hash_changes > 0 {
        issues.push(DiagnosticIssue {
            kind: "system_prefix_changed".to_string(),
            severity: DiagnosticSeverity::Warning,
            message: format!(
                "stable system/canonical prefix changed {} times within the same agent/model",
                cache.system_prefix_hash_changes
            ),
        });
    }
    if cache.tools_schema_hash_changes > 0 {
        issues.push(DiagnosticIssue {
            kind: "tools_schema_changed".to_string(),
            severity: DiagnosticSeverity::Warning,
            message: format!(
                "tool schema changed {} times within the same agent/model",
                cache.tools_schema_hash_changes
            ),
        });
    }
    if cache.read_rate.is_some_and(|rate| rate < 0.2) && usage.total_input_tokens >= 1_024 {
        issues.push(DiagnosticIssue {
            kind: "low_cache_read_rate".to_string(),
            severity: DiagnosticSeverity::Info,
            message: "provider cache read rate is below 20%; inspect component hash changes"
                .to_string(),
        });
    }
    let relative_limit = context.latest_context_limit_tokens / 4;
    let warning_limit = if relative_limit == 0 {
        PROTECTED_ABSOLUTE_WARNING_TOKENS
    } else {
        relative_limit.min(PROTECTED_ABSOLUTE_WARNING_TOKENS)
    };
    if context.max_protected_context_tokens > warning_limit {
        issues.push(DiagnosticIssue {
            kind: "protected_context_over_budget".to_string(),
            severity: DiagnosticSeverity::Warning,
            message: format!(
                "protected context peaked at ~{} tokens, above the ~{} token warning budget",
                context.max_protected_context_tokens, warning_limit
            ),
        });
    }
    issues
}

fn run_status_name(status: RunStatus) -> &'static str {
    match status {
        RunStatus::Pending => "pending",
        RunStatus::Running => "running",
        RunStatus::Completed => "completed",
        RunStatus::Failed => "failed",
        RunStatus::Cancelled => "cancelled",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use echo_agent::llm::cache::PromptCacheFingerprint;
    use echo_agent::trace::{InMemoryRunStore, LlmContextBreakdown, RunTimings, TokenUsage};

    fn run_with_calls(id: &str, events: Vec<RunEvent>) -> Run {
        let mut run = Run {
            run_id: id.to_string(),
            parent_run_id: Some("task-run".to_string()),
            agent_name: "main".to_string(),
            model: "model".to_string(),
            provider: Some("provider".to_string()),
            turn_id: Some("turn".to_string()),
            execution_id: None,
            session_id: "session".to_string(),
            status: RunStatus::Completed,
            input: "input".to_string(),
            events: Vec::new(),
            final_output: Some("ok".to_string()),
            error: None,
            token_usage: TokenUsage::default(),
            timings: RunTimings::default(),
            started_at: Utc::now(),
            finished_at: Some(Utc::now()),
        };
        for event in events {
            run.push_event(event);
        }
        run
    }

    fn llm_event(usage_reported: bool, protected: usize) -> RunEvent {
        RunEvent::LlmCall {
            messages: 3,
            prompt_tokens: 1_200,
            completion_tokens: 80,
            cached_prompt_tokens: 900,
            cache_creation_prompt_tokens: 20,
            usage_reported,
            estimated_context_tokens: 1_100,
            protected_context_tokens: protected,
            protected_message_count: 2,
            context_limit_tokens: 100_000,
            context_breakdown: LlmContextBreakdown {
                system_tokens: 200,
                user_tokens: 300,
                assistant_tokens: 300,
                tool_tokens: 200,
                summary_tokens: 50,
                memory_tokens: 50,
            },
            cache_fingerprint: PromptCacheFingerprint {
                stable_prefix_hash: "stable".to_string(),
                system_prefix_hash: "system".to_string(),
                tools_schema_hash: "tools".to_string(),
                history_hash: "history".to_string(),
                stable_prefix_message_count: 2,
                tool_count: 4,
            },
            duration_ms: 10,
        }
    }

    #[test]
    fn provider_totals_exclude_missing_usage_estimates() {
        let diagnostics = build_run_diagnostics(
            "task-run",
            vec![run_with_calls(
                "trace-1",
                vec![llm_event(true, 100), llm_event(false, 100)],
            )],
            None,
        );

        assert_eq!(diagnostics.usage.provider_reported_calls, 1);
        assert_eq!(diagnostics.usage.calls_missing_usage, 1);
        assert_eq!(diagnostics.usage.total_input_tokens, 1_200);
        let second_call = diagnostics
            .traces
            .first()
            .and_then(|trace| trace.llm_calls.get(1));
        assert!(second_call.is_some_and(|call| call.input_tokens.is_none()));
        assert_eq!(
            second_call.map(|call| call.source),
            Some(UsageSource::Estimated)
        );
    }

    #[test]
    fn protected_context_warning_uses_bounded_threshold() {
        let diagnostics = build_run_diagnostics(
            "task-run",
            vec![run_with_calls("trace-1", vec![llm_event(true, 30_000)])],
            None,
        );

        assert!(
            diagnostics
                .issues
                .iter()
                .any(|issue| issue.kind == "protected_context_over_budget")
        );
    }

    #[tokio::test]
    async fn parent_run_projection_uses_one_durable_diagnostic_contract()
    -> Result<(), Box<dyn std::error::Error>> {
        let store = InMemoryRunStore::new();
        let first = run_with_calls("trace-1", vec![llm_event(true, 100)]);
        let mut changed_call = llm_event(true, 120);
        if let RunEvent::LlmCall {
            cache_fingerprint, ..
        } = &mut changed_call
        {
            cache_fingerprint.system_prefix_hash = "system-changed".to_string();
            cache_fingerprint.stable_prefix_hash = "stable-changed".to_string();
        }
        let second = run_with_calls(
            "trace-2",
            vec![
                changed_call,
                RunEvent::ContextCompression {
                    source: "auto".to_string(),
                    before_messages: 20,
                    after_messages: 8,
                    before_tokens: 8_000,
                    after_tokens: 3_000,
                    protected_context_tokens: 500,
                    protected_message_count: 2,
                },
            ],
        );
        store.save(first).await?;
        store.save(second).await?;

        let summaries = list_diagnostic_runs(&store).await?;
        let summary = summaries.first().ok_or("diagnostic summary missing")?;
        assert_eq!(summary.diagnostic_id, "task-run");
        assert_eq!(summary.trace_count, 2);

        let diagnostics = load_run_diagnostics(&store, "task-run", None)
            .await?
            .ok_or("run diagnostics missing")?;
        assert_eq!(diagnostics.traces.len(), 2);
        assert_eq!(diagnostics.cache.system_prefix_hash_changes, 1);
        assert_eq!(diagnostics.cache.stable_prefix_hash_changes, 1);
        assert_eq!(diagnostics.compressions.len(), 1);
        let formatted = format_run_diagnostics(&diagnostics);
        assert!(formatted.contains("provider usage calls: 2"));
        assert!(formatted.contains("8000 -> 3000 tokens"));
        Ok(())
    }
}
