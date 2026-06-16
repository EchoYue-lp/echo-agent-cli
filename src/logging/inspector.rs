//! LLM 请求/响应检查器
//!
//! 在详细模式下记录和展示 Agent 与 LLM 之间的完整通信。

use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::sync::Mutex;

/// 单次 LLM 交互记录
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmCallRecord {
    /// 调用时间
    pub timestamp: String,
    /// 调用 ID
    pub call_id: String,
    /// 模型名
    pub model: String,
    /// 请求消息（序列化的 JSON）
    pub request_body: String,
    /// 响应内容
    pub response_body: String,
    /// 请求 Token 数
    pub prompt_tokens: Option<u32>,
    /// 响应 Token 数
    pub completion_tokens: Option<u32>,
    /// 耗时 (毫秒)
    pub duration_ms: u64,
    /// 是否成功
    pub success: bool,
    /// 错误信息
    pub error: Option<String>,
}

/// LLM 通信检查器
///
/// 收集和检索 Agent 与 LLM 的交互记录。
pub struct LlmInspector {
    /// 调用记录
    records: Mutex<Vec<LlmCallRecord>>,
    /// 最大保留记录数
    max_records: usize,
    /// 是否启用记录
    enabled: bool,
}

impl LlmInspector {
    pub fn new() -> Self {
        Self {
            records: Mutex::new(Vec::new()),
            max_records: 500,
            enabled: false,
        }
    }

    /// 启用/禁用检查器
    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }

    /// 是否已启用
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// 记录一次 LLM 调用
    pub fn record(&self, record: LlmCallRecord) {
        if !self.enabled {
            return;
        }
        if let Ok(mut records) = self.records.lock() {
            records.push(record);
            if records.len() > self.max_records {
                records.remove(0);
            }
        }
    }

    /// 获取所有记录
    pub fn all_records(&self) -> Vec<LlmCallRecord> {
        self.records.lock().map(|r| r.clone()).unwrap_or_default()
    }

    /// 获取最近 N 条记录
    pub fn recent(&self, n: usize) -> Vec<LlmCallRecord> {
        let records = self.all_records();
        let start = records.len().saturating_sub(n);
        records[start..].to_vec()
    }

    /// 获取成功/失败统计
    pub fn stats(&self) -> InspectorStats {
        let records = self.all_records();
        let total = records.len();
        let success = records.iter().filter(|r| r.success).count();
        let failed = total - success;
        let total_prompt_tokens: u32 = records.iter().filter_map(|r| r.prompt_tokens).sum();
        let total_completion_tokens: u32 = records.iter().filter_map(|r| r.completion_tokens).sum();
        let total_duration_ms: u64 = records.iter().map(|r| r.duration_ms).sum();
        let avg_duration_ms = if total > 0 {
            total_duration_ms / total as u64
        } else {
            0
        };

        InspectorStats {
            total_calls: total,
            success_calls: success,
            failed_calls: failed,
            total_prompt_tokens,
            total_completion_tokens,
            total_duration_ms,
            avg_duration_ms,
        }
    }

    /// 清空记录
    pub fn clear(&self) {
        if let Ok(mut records) = self.records.lock() {
            records.clear();
        }
    }
}

impl Default for LlmInspector {
    fn default() -> Self {
        Self::new()
    }
}

/// 检查器统计
#[derive(Debug, Clone)]
pub struct InspectorStats {
    pub total_calls: usize,
    pub success_calls: usize,
    pub failed_calls: usize,
    pub total_prompt_tokens: u32,
    pub total_completion_tokens: u32,
    pub total_duration_ms: u64,
    pub avg_duration_ms: u64,
}

/// 便捷函数：创建调用记录
#[allow(clippy::too_many_arguments)]
pub fn create_record(
    model: &str,
    request: &str,
    response: &str,
    prompt_tokens: Option<u32>,
    completion_tokens: Option<u32>,
    duration_ms: u64,
    success: bool,
    error: Option<String>,
) -> LlmCallRecord {
    LlmCallRecord {
        timestamp: Utc::now().to_rfc3339(),
        call_id: uuid::Uuid::new_v4().to_string(),
        model: model.to_string(),
        request_body: request.to_string(),
        response_body: response.to_string(),
        prompt_tokens,
        completion_tokens,
        duration_ms,
        success,
        error,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_inspector_record_and_stats() {
        let mut inspector = LlmInspector::new();
        inspector.set_enabled(true);

        inspector.record(create_record(
            "test",
            "{}",
            "{}",
            Some(10),
            Some(20),
            100,
            true,
            None,
        ));
        inspector.record(create_record(
            "test",
            "{}",
            "{}",
            Some(5),
            Some(15),
            200,
            false,
            Some("err".into()),
        ));

        let stats = inspector.stats();
        assert_eq!(stats.total_calls, 2);
        assert_eq!(stats.success_calls, 1);
        assert_eq!(stats.failed_calls, 1);
        assert_eq!(stats.total_prompt_tokens, 15);
        assert_eq!(stats.total_completion_tokens, 35);
    }

    #[test]
    fn test_inspector_disabled() {
        let inspector = LlmInspector::new();
        // Not enabled, should not record
        inspector.record(create_record("test", "{}", "{}", None, None, 0, true, None));
        assert_eq!(inspector.stats().total_calls, 0);
    }
}
