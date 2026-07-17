use chrono::{DateTime, Utc};
use echo_agent::trace::LlmContextBreakdown;
use serde::Serialize;

use crate::project::prompt::PromptAssembly;

#[derive(Debug, Clone, Serialize)]
pub struct DiagnosticRunSummary {
    pub diagnostic_id: String,
    pub parent_run_id: Option<String>,
    pub trace_count: usize,
    pub status: String,
    pub input_preview: String,
    #[serde(with = "echo_agent::utils::time::local_rfc3339")]
    pub started_at: DateTime<Utc>,
    #[serde(with = "echo_agent::utils::time::option_local_rfc3339")]
    pub finished_at: Option<DateTime<Utc>>,
    pub agents: Vec<String>,
    pub models: Vec<String>,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub total_cached_input_tokens: u64,
    pub llm_calls: usize,
    pub calls_missing_usage: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct RunDiagnostics {
    pub diagnostic_id: String,
    pub parent_run_id: Option<String>,
    pub traces: Vec<TraceInvocationDiagnostic>,
    pub usage: RunUsageDiagnostic,
    pub cache: CacheDiagnostic,
    pub context: ContextDiagnostic,
    pub compressions: Vec<CompressionDiagnostic>,
    pub issues: Vec<DiagnosticIssue>,
    pub prompt_assembly: Option<PromptAssembly>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TraceInvocationDiagnostic {
    pub trace_run_id: String,
    pub agent_name: String,
    pub model: String,
    pub provider: Option<String>,
    pub turn_id: Option<String>,
    pub execution_id: Option<String>,
    pub status: String,
    #[serde(with = "echo_agent::utils::time::local_rfc3339")]
    pub started_at: DateTime<Utc>,
    #[serde(with = "echo_agent::utils::time::option_local_rfc3339")]
    pub finished_at: Option<DateTime<Utc>>,
    pub llm_calls: Vec<LlmCallDiagnostic>,
}

#[derive(Debug, Clone, Serialize)]
pub struct LlmCallDiagnostic {
    pub sequence: usize,
    pub source: UsageSource,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cached_input_tokens: Option<u64>,
    pub cache_creation_input_tokens: Option<u64>,
    pub estimated_context_tokens: usize,
    pub protected_context_tokens: usize,
    pub protected_message_count: usize,
    pub context_limit_tokens: usize,
    pub context_breakdown: LlmContextBreakdown,
    pub stable_prefix_hash: String,
    pub system_prefix_hash: String,
    pub tools_schema_hash: String,
    pub history_hash: String,
    pub message_count: usize,
    pub tool_count: usize,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UsageSource {
    Provider,
    Estimated,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct RunUsageDiagnostic {
    pub provider_reported_calls: usize,
    pub calls_missing_usage: usize,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub total_cached_input_tokens: u64,
    pub total_cache_creation_input_tokens: u64,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct CacheDiagnostic {
    pub read_rate: Option<f64>,
    pub system_prefix_hash_changes: usize,
    pub tools_schema_hash_changes: usize,
    pub stable_prefix_hash_changes: usize,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct ContextDiagnostic {
    pub latest_provider_input_tokens: Option<u64>,
    pub latest_estimated_context_tokens: usize,
    pub latest_context_limit_tokens: usize,
    pub latest_breakdown: LlmContextBreakdown,
    pub max_protected_context_tokens: usize,
    pub max_protected_message_count: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct CompressionDiagnostic {
    pub trace_run_id: String,
    pub sequence: usize,
    pub source: String,
    pub before_messages: usize,
    pub after_messages: usize,
    pub before_tokens: usize,
    pub after_tokens: usize,
    pub protected_context_tokens: usize,
    pub protected_message_count: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct DiagnosticIssue {
    pub kind: String,
    pub severity: DiagnosticSeverity,
    pub message: String,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticSeverity {
    Info,
    Warning,
    Critical,
}
