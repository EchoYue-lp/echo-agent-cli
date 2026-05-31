//! 上下文压缩 API
//!
//! 提供手动触发压缩和获取压缩统计功能。
//!
//! # 背景
//!
//! 随着 Agent 对话轮次增加，上下文（消息历史）会不断膨胀。
//! 当 token 数接近或超过模型限制时，需要压缩上下文以释放空间。
//!
//! # API 端点
//!
//! | 端点 | 方法 | 说明 |
//! |------|------|------|
//! | `/api/compress` | POST | 手动触发压缩 |
//! | `/api/compress/stats` | GET | 获取压缩统计 |
//!
//! # 压缩策略
//!
//! 当前使用滑动窗口策略：保留最近的 N 条消息，丢弃较早的消息。

use axum::{
    Json, debug_handler,
    extract::State,
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::error::WebError;
use crate::state::AppState;
use echo_agent::prelude::SlidingWindowCompressor;

// ── 请求类型 ─────────────────────────────────────────────────

/// 压缩请求
///
/// # 示例
///
/// ```json
/// {
///   "keep_messages": 20
/// }
/// ```
#[derive(Debug, Deserialize)]
pub struct CompressRequest {
    /// 滑动窗口保留的消息数
    /// 默认为 10，即保留最近 10 条消息
    #[serde(default = "default_keep_messages")]
    pub keep_messages: usize,
}

fn default_keep_messages() -> usize {
    10
}

// ── 响应类型 ─────────────────────────────────────────────────

/// 压缩响应
///
/// 包含压缩前后的统计信息。
#[derive(Debug, Serialize)]
pub struct CompressResponse {
    /// 是否成功
    pub success: bool,
    /// 压缩前消息数
    pub messages_before: usize,
    /// 压缩后消息数
    pub messages_after: usize,
    /// 节省的 token 数
    pub tokens_saved: usize,
    /// 结果消息
    pub message: String,
}

/// 压缩统计响应
///
/// 当前上下文的使用情况和压缩建议。
#[derive(Debug, Serialize)]
pub struct CompressionStatsResponse {
    /// 当前 token 数（估算值）
    pub current_tokens: usize,
    /// token 上限
    pub token_limit: usize,
    /// 当前消息数
    pub message_count: usize,
    /// 压缩比例（当前/上限）
    pub compression_ratio: f32,
    /// 是否需要压缩
    pub needs_compression: bool,
}

// ── API 处理函数 ─────────────────────────────────────────────────

/// POST /api/compress - 手动触发上下文压缩
///
/// 使用滑动窗口策略压缩对话历史。
///
/// # 请求体
///
/// ```json
/// {
///   "keep_messages": 20
/// }
/// ```
///
/// # 响应
///
/// ```json
/// {
///   "success": true,
///   "messages_before": 50,
///   "messages_after": 20,
///   "tokens_saved": 3000,
///   "message": "压缩完成: 50 -> 20 消息, 节省 30 tokens"
/// }
/// ```
#[cfg_attr(debug_assertions, debug_handler)]
pub async fn compress(
    State(state): State<Arc<AppState>>,
    Json(req): Json<CompressRequest>,
) -> Response {
    let compressor = SlidingWindowCompressor::new(req.keep_messages);

    let result = state
        .connection
        .agent
        .read_async(|agent| {
            Box::pin(async move { agent.force_compress_with_hooks(&compressor, "manual").await })
        })
        .await;

    match result {
        Ok(stats) => Json(CompressResponse {
            success: true,
            messages_before: stats.before_count,
            messages_after: stats.after_count,
            tokens_saved: stats.before_tokens.saturating_sub(stats.after_tokens),
            message: format!(
                "压缩完成: {} -> {} 消息, 节省 {} tokens",
                stats.before_count, stats.after_count, stats.evicted
            ),
        })
        .into_response(),
        Err(e) => WebError::Internal(format!("压缩失败: {}", e)).into_response(),
    }
}

/// GET /api/compress/stats - 获取压缩统计信息
///
/// 返回当前上下文的使用情况和建议。
///
/// # 响应
///
/// ```json
/// {
///   "current_tokens": 5000,
///   "token_limit": 8000,
///   "message_count": 25,
///   "compression_ratio": 0.625,
///   "needs_compression": false
/// }
/// ```
#[cfg_attr(debug_assertions, debug_handler)]
pub async fn get_compression_stats(State(state): State<Arc<AppState>>) -> Response {
    let (message_count, current_tokens, token_limit) = state
        .connection
        .agent
        .read_async(|agent| {
            Box::pin(async move {
                let (mc, ct) = agent.context_stats().await;
                let tl = agent.config().get_token_limit();
                (mc, ct, tl)
            })
        })
        .await;

    // 计算压缩比例（当前 token 使用率）
    let compression_ratio = if token_limit > 0 {
        current_tokens as f32 / token_limit as f32
    } else {
        0.0
    };

    // 当使用率超过 80% 时建议压缩
    let needs_compression = compression_ratio > 0.8;

    Json(CompressionStatsResponse {
        current_tokens,
        token_limit,
        message_count,
        compression_ratio,
        needs_compression,
    })
    .into_response()
}

// ── 单元测试 ─────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compress_request_deserialize() {
        let json = r#"{"keep_messages": 20}"#;
        let req: CompressRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.keep_messages, 20);
    }

    #[test]
    fn test_compress_request_default() {
        let json = r#"{}"#;
        let req: CompressRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.keep_messages, 10);
    }

    #[test]
    fn test_compress_response_serialize() {
        let resp = CompressResponse {
            success: true,
            messages_before: 50,
            messages_after: 20,
            tokens_saved: 3000,
            message: "压缩完成".to_string(),
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"success\":true"));
        assert!(json.contains("\"messages_before\":50"));
    }

    #[test]
    fn test_compression_stats_response_serialize() {
        let resp = CompressionStatsResponse {
            current_tokens: 5000,
            token_limit: 8000,
            message_count: 25,
            compression_ratio: 0.625,
            needs_compression: false,
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"current_tokens\":5000"));
        assert!(json.contains("\"needs_compression\":false"));
    }

    #[test]
    fn test_needs_compression_threshold() {
        // 当 ratio > 0.8 时需要压缩
        let ratio = 0.85f32;
        let needs = ratio > 0.8;
        assert!(needs);

        let ratio = 0.75f32;
        let needs = ratio > 0.8;
        assert!(!needs);
    }
}
