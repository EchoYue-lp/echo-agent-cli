//! Unified event types for the conversation timeline.
//!
//! Both normal chat and TaskRuntime execution emit these events. The frontend
//! renders them as a single timeline in the main conversation area.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::super::router::{TaskRouteDecision, TaskRouteKind};
use super::super::types::InteractionMode;

/// A single event in the unified conversation timeline.
///
/// The frontend subscribes to `conversation://event` and appends each event
/// to a unified timeline that replaces the dual chat / TaskRuntimeMainPanel
/// rendering.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
#[ts(export, rename = "ConversationRuntimeEvent")]
pub enum ConversationRuntimeEvent {
    /// Router classified the message and selected a route.
    RouteDecision {
        route: String,
        confidence: f32,
        reason: String,
        matched_feedback_pattern: Option<String>,
        suggested_workers: Vec<String>,
        interaction_mode: String,
    },
    /// The agent started thinking.
    InitialThinking {
        worker_id: Option<String>,
    },
    /// A worker started execution.
    WorkerStarted {
        worker_id: String,
        agent_role: String,
        title: String,
        task_description: String,
    },
    /// A worker made a tool call.
    WorkerToolCall {
        worker_id: String,
        tool_name: String,
        tool_args: serde_json::Value,
        success: Option<bool>,
    },
    /// A worker produced a result.
    WorkerResult {
        worker_id: String,
        summary: String,
        files_changed: Vec<String>,
    },
    /// LLM usage event (from main agent or worker).
    LlmUsage {
        model: String,
        input_tokens: u64,
        output_tokens: u64,
        cached_input_tokens: u64,
        cache_creation_input_tokens: u64,
        usage_reported: bool,
        worker_id: Option<String>,
    },
    /// Final answer from the conversation.
    FinalAnswer {
        content: String,
        usage_summary: Option<serde_json::Value>,
    },
    /// Human-in-the-loop approval request.
    ApprovalRequest {
        request_id: String,
        tool_name: String,
        prompt: String,
    },
    /// An error at any stage.
    Error {
        stage: String,
        message: String,
        worker_id: Option<String>,
    },
}

impl ConversationRuntimeEvent {
    /// Serialize to a JSON value suitable for Tauri event emission.
    pub fn to_emit_json(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or_default()
    }
}
