//! 追踪事件收集器
//!
//! 收集 Agent 执行过程中的追踪事件，提供查询和统计功能。

use std::collections::HashMap;

use chrono::Utc;
use tokio::sync::{RwLock, broadcast};

use super::types::{TraceEvent, TraceKind, TraceSummary};

/// 追踪事件收集器。
///
/// 线程安全，支持实时订阅。
pub struct TraceCollector {
    /// 按 session_id 分组的事件存储。
    events: RwLock<HashMap<String, Vec<TraceEvent>>>,
    /// 实时事件广播通道。
    tx: broadcast::Sender<TraceEvent>,
    /// 最大存储事件数（每个 session）。
    max_events_per_session: usize,
}

impl TraceCollector {
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(256);
        Self {
            events: RwLock::new(HashMap::new()),
            tx,
            max_events_per_session: 1000,
        }
    }

    /// 记录一个追踪事件。
    pub async fn record(&self, session_id: &str, event: TraceEvent) {
        // Broadcast to subscribers
        let _ = self.tx.send(event.clone());

        // Store
        let mut events = self.events.write().await;
        let session_events = events.entry(session_id.to_string()).or_default();
        session_events.push(event);

        // Trim if over limit
        if session_events.len() > self.max_events_per_session {
            let excess = session_events.len() - self.max_events_per_session;
            session_events.drain(0..excess);
        }
    }

    /// 订阅实时事件流。
    pub fn subscribe(&self) -> broadcast::Receiver<TraceEvent> {
        self.tx.subscribe()
    }

    /// 获取指定 session 的所有事件。
    pub async fn get_events(&self, session_id: &str) -> Vec<TraceEvent> {
        let events = self.events.read().await;
        events.get(session_id).cloned().unwrap_or_default()
    }

    /// 获取指定 session 的摘要统计。
    pub async fn get_summary(&self, session_id: &str) -> Option<TraceSummary> {
        let events = self.events.read().await;
        let session_events = events.get(session_id)?;

        if session_events.is_empty() {
            return None;
        }

        let mut llm_calls = 0;
        let mut total_input_tokens = 0u64;
        let mut total_output_tokens = 0u64;
        let mut total_cached_input_tokens = 0u64;
        let mut total_cache_creation_input_tokens = 0u64;
        let mut llm_calls_missing_usage = 0usize;
        let mut tool_calls = 0;
        let mut tool_successes = 0;
        let mut agent_steps = 0;

        for event in session_events {
            match &event.kind {
                TraceKind::LlmCall {
                    input_tokens,
                    output_tokens,
                    cached_input_tokens,
                    cache_creation_input_tokens,
                    usage_reported,
                    ..
                } => {
                    llm_calls += 1;
                    total_input_tokens += input_tokens;
                    total_output_tokens += output_tokens;
                    total_cached_input_tokens += cached_input_tokens;
                    total_cache_creation_input_tokens += cache_creation_input_tokens;
                    if !usage_reported {
                        llm_calls_missing_usage += 1;
                    }
                }
                TraceKind::ToolCall { success, .. } => {
                    tool_calls += 1;
                    if *success {
                        tool_successes += 1;
                    }
                }
                TraceKind::AgentStep { .. } => {
                    agent_steps += 1;
                }
                _ => {}
            }
        }

        let first_ts = session_events
            .first()
            .map(|e| e.timestamp)
            .unwrap_or_else(Utc::now);
        let last_ts = session_events
            .last()
            .map(|e| e.timestamp)
            .unwrap_or_else(Utc::now);
        let total_duration_ms = (last_ts - first_ts).num_milliseconds().max(0) as u64;

        Some(TraceSummary {
            session_id: session_id.to_string(),
            total_duration_ms,
            llm_calls,
            total_input_tokens,
            total_output_tokens,
            total_cached_input_tokens,
            total_cache_creation_input_tokens,
            llm_calls_missing_usage,
            tool_calls,
            tool_success_rate: if tool_calls > 0 {
                tool_successes as f64 / tool_calls as f64
            } else {
                1.0
            },
            agent_steps,
            events: session_events.clone(),
        })
    }

    /// 列出所有 session ID。
    pub async fn list_sessions(&self) -> Vec<String> {
        let events = self.events.read().await;
        events.keys().cloned().collect()
    }

    /// 清除指定 session 的事件。
    pub async fn clear_session(&self, session_id: &str) {
        let mut events = self.events.write().await;
        events.remove(session_id);
    }

    /// 清除所有事件。
    pub async fn clear_all(&self) {
        let mut events = self.events.write().await;
        events.clear();
    }
}

impl Default for TraceCollector {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_record_and_get() {
        let collector = TraceCollector::new();
        let event = TraceEvent {
            timestamp: Utc::now(),
            kind: TraceKind::AgentStep {
                step_number: 1,
                thought_preview: Some("thinking...".into()),
            },
            duration_ms: Some(100),
            metadata: HashMap::new(),
        };

        collector.record("session-1", event).await;
        let events = collector.get_events("session-1").await;
        assert_eq!(events.len(), 1);
    }

    #[tokio::test]
    async fn test_summary() {
        let collector = TraceCollector::new();

        collector
            .record(
                "s1",
                TraceEvent {
                    timestamp: Utc::now(),
                    kind: TraceKind::LlmCall {
                        model: "qwen".into(),
                        input_tokens: 100,
                        output_tokens: 50,
                        cached_input_tokens: 80,
                        cache_creation_input_tokens: 0,
                        usage_reported: true,
                    },
                    duration_ms: Some(200),
                    metadata: HashMap::new(),
                },
            )
            .await;

        collector
            .record(
                "s1",
                TraceEvent {
                    timestamp: Utc::now(),
                    kind: TraceKind::ToolCall {
                        tool: "shell".into(),
                        success: true,
                        error: None,
                    },
                    duration_ms: Some(50),
                    metadata: HashMap::new(),
                },
            )
            .await;

        let summary = collector.get_summary("s1").await.unwrap();
        assert_eq!(summary.llm_calls, 1);
        assert_eq!(summary.total_input_tokens, 100);
        assert_eq!(summary.total_cached_input_tokens, 80);
        assert_eq!(summary.llm_calls_missing_usage, 0);
        assert_eq!(summary.tool_calls, 1);
        assert_eq!(summary.tool_success_rate, 1.0);
    }
}
