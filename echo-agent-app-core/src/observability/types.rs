//! 可观测性数据类型定义

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// 单个追踪事件。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceEvent {
    /// 事件时间戳。
    #[serde(with = "echo_agent::utils::time::local_rfc3339")]
    pub timestamp: DateTime<Utc>,
    /// 事件类型。
    pub kind: TraceKind,
    /// 耗时（毫秒），如果适用。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    /// 附加元数据。
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub metadata: HashMap<String, serde_json::Value>,
}

/// Content fingerprint for tracking cache stability across LLM calls.
///
/// Each hash covers the canonical content of one dimension that affects
/// prompt-cache hit rate: system prompt, tools schema, working-directory,
/// and subagent prompt. When the hash changes between calls, the provider
/// cannot reuse the prior cache entry.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ContentFingerprint {
    /// Truncated hash (first 16 hex chars of SHA-256).
    pub hash: String,
    /// First 80 chars of the content for quick visual scanning.
    pub preview: String,
}

/// 追踪事件类型。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TraceKind {
    /// LLM API 调用。
    LlmCall {
        model: String,
        input_tokens: u64,
        output_tokens: u64,
        #[serde(default)]
        cached_input_tokens: u64,
        #[serde(default)]
        cache_creation_input_tokens: u64,
        #[serde(default)]
        usage_reported: bool,
        /// SHA-256 hash of system prompt content (first 16 hex chars).
        /// Changes when system prompt, memory injection, or hook output varies.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        system_prompt_hash: Option<String>,
        /// SHA-256 hash of sorted tool names + parameter JSON.
        /// Changes when tools are added, removed, or reordered.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tools_schema_hash: Option<String>,
        /// SHA-256 hash of current working directory + workspace root.
        /// Changes when the agent switches workspaces mid-session.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cwd_hash: Option<String>,
        /// SHA-256 hash of the worker/sub-agent prompt template.
        /// None for main-agent calls; changes when subagent prompts vary.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        worker_prompt_hash: Option<String>,
        /// Provider ID string (e.g. "deepseek", "openai", "anthropic").
        /// Different providers never share prompt cache.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider: Option<String>,
    },
    /// 工具调用。
    ToolCall {
        tool: String,
        success: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    /// Agent 推理步骤。
    AgentStep {
        step_number: u32,
        thought_preview: Option<String>,
    },
    /// Pipeline 阶段。
    PipelineStage { pipeline: String, stage: String },
    /// 记忆访问。
    MemoryAccess {
        operation: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        results_count: Option<usize>,
    },
    /// MCP 服务调用。
    McpCall { server: String, method: String },
    /// 上下文压缩。
    ContextCompression {
        before_messages: usize,
        after_messages: usize,
        before_tokens: usize,
        after_tokens: usize,
    },
}

/// 追踪摘要 — 一次完整执行的统计信息。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceSummary {
    /// 会话/执行 ID。
    pub session_id: String,
    /// 总耗时（毫秒）。
    pub total_duration_ms: u64,
    /// LLM 调用次数。
    pub llm_calls: usize,
    /// 总 input tokens。
    pub total_input_tokens: u64,
    /// 总 output tokens。
    pub total_output_tokens: u64,
    /// Provider 侧 prompt cache 命中的 input tokens。
    #[serde(default)]
    pub total_cached_input_tokens: u64,
    /// Provider 侧 prompt cache 写入的 input tokens。
    #[serde(default)]
    pub total_cache_creation_input_tokens: u64,
    /// 没有返回 usage metadata 的 LLM 请求数。
    #[serde(default)]
    pub llm_calls_missing_usage: usize,
    /// 工具调用次数。
    pub tool_calls: usize,
    /// 工具调用成功率。
    pub tool_success_rate: f64,
    /// Agent 推理步骤数。
    pub agent_steps: usize,
    /// 事件列表。
    pub events: Vec<TraceEvent>,
}
