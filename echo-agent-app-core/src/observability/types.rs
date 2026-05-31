//! 可观测性数据类型定义

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// 单个追踪事件。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceEvent {
    /// 事件时间戳。
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

/// 追踪事件类型。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TraceKind {
    /// LLM API 调用。
    LlmCall {
        model: String,
        input_tokens: u64,
        output_tokens: u64,
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
    PipelineStage {
        pipeline: String,
        stage: String,
    },
    /// 记忆访问。
    MemoryAccess {
        operation: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        results_count: Option<usize>,
    },
    /// MCP 服务调用。
    McpCall {
        server: String,
        method: String,
    },
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
    /// 工具调用次数。
    pub tool_calls: usize,
    /// 工具调用成功率。
    pub tool_success_rate: f64,
    /// Agent 推理步骤数。
    pub agent_steps: usize,
    /// 事件列表。
    pub events: Vec<TraceEvent>,
}
