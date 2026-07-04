//! Cache diagnostics — explains WHY cache hit rate is low.
//!
//! Computes per-session diagnostics from `TraceEvent` sequences by
//! analysing content-fingerprint stability across LLM calls.

use super::types::{TraceEvent, TraceKind};
use serde::{Deserialize, Serialize};

/// Complete cache diagnostics for a session or time window.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheDiagnostics {
    /// Overall cache read rate: cached_input_tokens / input_tokens.
    pub overall_read_rate: f64,
    /// Total input tokens across all calls.
    pub total_input_tokens: u64,
    /// Total cached (read) input tokens.
    pub total_cached_input_tokens: u64,
    /// Total cache-creation (write) input tokens.
    pub total_cache_creation_input_tokens: u64,
    /// Total LLM calls in this window.
    pub total_llm_calls: usize,
    /// Calls missing usage_reported.
    pub calls_missing_usage: usize,
    /// Distinct models observed (caches don't share across models).
    pub distinct_models: usize,
    /// Diagnostic issues found, ordered by severity.
    pub issues: Vec<CacheIssue>,
    /// Actionable fix suggestions.
    pub suggested_fixes: Vec<String>,
    /// Per-dimension fingerprint change counts (for cache diff diagnostics).
    pub fingerprint_changes: CacheFingerprintChanges,
}

/// Which dimensions changed across LLM calls (cache miss root cause).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CacheFingerprintChanges {
    /// How many times the system prompt hash changed.
    pub system_prompt_hash_changes: usize,
    /// How many times the tools schema hash changed.
    pub tools_schema_hash_changes: usize,
    /// How many times the cwd hash changed.
    pub cwd_hash_changes: usize,
    /// How many times the subagent prompt hash changed.
    pub worker_prompt_hash_changes: usize,
    /// How many distinct providers were used.
    pub distinct_providers: usize,
}

/// A single cache-stability issue detected in the trace.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheIssue {
    pub kind: CacheIssueKind,
    pub severity: IssueSeverity,
    pub message: String,
    /// How many LLM calls are affected by this issue.
    pub affected_calls: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CacheIssueKind {
    SystemPrefixChanged,
    ToolsSchemaChanged,
    CwdOrWorkspaceChanged,
    WorkerPromptVariation,
    MissingUsageData,
    NearZeroCachedTokens,
    MultiModelNoSharedCache,
    CacheWriteHigherThanRead,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IssueSeverity {
    Info,
    Warning,
    Critical,
}

/// Compute cache diagnostics from a sequence of trace events.
///
/// The function groups consecutive `LlmCall` events, tracks fingerprint
/// changes, and generates human-readable issues + fix suggestions.
pub fn compute_cache_diagnostics(events: &[TraceEvent]) -> CacheDiagnostics {
    let llm_calls: Vec<&TraceEvent> = events
        .iter()
        .filter(|e| matches!(e.kind, TraceKind::LlmCall { .. }))
        .collect();

    // ── Aggregate token counts ──────────────────────────────────────────
    let mut total_input_tokens = 0u64;
    let mut total_cached_input_tokens = 0u64;
    let mut total_cache_creation_input_tokens = 0u64;
    let mut calls_missing_usage = 0usize;
    let mut models = std::collections::HashSet::new();
    let mut near_zero_cached = 0usize;

    let mut system_hashes = std::collections::HashSet::new();
    let mut tools_hashes = std::collections::HashSet::new();
    let mut cwd_hashes = std::collections::HashSet::new();
    let mut worker_hashes = std::collections::HashSet::new();
    let mut calls_with_worker_prompt = 0usize;
    let mut calls_with_cache_write = 0usize;

    for event in &llm_calls {
        let TraceKind::LlmCall {
            input_tokens,
            cached_input_tokens,
            cache_creation_input_tokens,
            usage_reported,
            model,
            system_prompt_hash,
            tools_schema_hash,
            cwd_hash,
            worker_prompt_hash,
            ..
        } = &event.kind
        else {
            continue;
        };
        total_input_tokens += *input_tokens;
        total_cached_input_tokens += *cached_input_tokens;
        total_cache_creation_input_tokens += *cache_creation_input_tokens;

        if !usage_reported {
            calls_missing_usage += 1;
        }

        models.insert(model.clone());

        if *input_tokens >= 2000 && *cached_input_tokens < (*input_tokens / 20) {
            near_zero_cached += 1;
        }

        if *cache_creation_input_tokens > 0 {
            calls_with_cache_write += 1;
        }

        if let Some(h) = system_prompt_hash {
            system_hashes.insert(h.clone());
        }
        if let Some(h) = tools_schema_hash {
            tools_hashes.insert(h.clone());
        }
        if let Some(h) = cwd_hash {
            cwd_hashes.insert(h.clone());
        }
        if let Some(h) = worker_prompt_hash {
            worker_hashes.insert(h.clone());
            calls_with_worker_prompt += 1;
        }
    }

    let overall_read_rate = if total_input_tokens > 0 {
        total_cached_input_tokens as f64 / total_input_tokens as f64
    } else {
        0.0
    };

    let total_llm_calls = llm_calls.len();

    // ── Build issues ────────────────────────────────────────────────────
    let mut issues = Vec::new();
    let mut suggested_fixes = Vec::new();

    // Missing usage data
    if calls_missing_usage > 0 {
        let all_missing = calls_missing_usage >= total_llm_calls;
        let (severity, msg) = if all_missing {
            (
                IssueSeverity::Critical,
                format!(
                    "全部 {total_llm_calls} 次 LLM 调用都没有返回 provider usage 元数据。\
                     缓存命中率无法计算。这通常意味着 provider 不支持 streaming \
                     stream_options.include_usage，或 SSE 解析中丢弃了 usage chunk。"
                ),
            )
        } else {
            (
                IssueSeverity::Warning,
                format!("{calls_missing_usage} 次请求缺少 provider usage，缓存命中率可能被低估。"),
            )
        };
        issues.push(CacheIssue {
            kind: CacheIssueKind::MissingUsageData,
            severity,
            message: msg,
            affected_calls: calls_missing_usage,
        });
        if all_missing {
            suggested_fixes.push(
                "检查 provider 是否支持 stream_options.include_usage；\
                 若不支持，可切换到非 streaming 调用以获取 usage 数据。"
                    .to_string(),
            );
        }
    }

    // System prefix changes
    let sys_changes = system_hashes.len().saturating_sub(1);
    if sys_changes > 0 {
        let severity = if sys_changes > 3 {
            IssueSeverity::Critical
        } else {
            IssueSeverity::Warning
        };
        issues.push(CacheIssue {
            kind: CacheIssueKind::SystemPrefixChanged,
            severity,
            message: format!(
                "System prompt 发生了 {sys_changes} 次变化（共 {total_llm_calls} 次调用）。\
                 频繁变化的 system prompt 会阻止 provider 复用上一轮的 prefix cache。"
            ),
            affected_calls: sys_changes + 1,
        });
        suggested_fixes.push(
            "稳定化 system prompt：检查是否每次调用都注入了变化的 cwd、memory 摘要或 hook 输出。\
             将这些动态内容放入 user message 而非 system message。"
                .to_string(),
        );
    }

    // Tools schema changes
    let tools_changes = tools_hashes.len().saturating_sub(1);
    if tools_changes > 0 {
        issues.push(CacheIssue {
            kind: CacheIssueKind::ToolsSchemaChanged,
            severity: IssueSeverity::Warning,
            message: format!(
                "Tools schema 发生了 {tools_changes} 次变化。Tools 定义顺序或参数的改变\
                 会导致整个 prompt prefix 被视为不同，cache 无法命中。"
            ),
            affected_calls: tools_changes + 1,
        });
        suggested_fixes.push(
            "确保 tools 列表的顺序固定（按名称排序），不要在每次调用时动态增删 tool。".to_string(),
        );
    }

    // CWD/workspace changes
    let cwd_changes = cwd_hashes.len().saturating_sub(1);
    if cwd_changes > 0 {
        issues.push(CacheIssue {
            kind: CacheIssueKind::CwdOrWorkspaceChanged,
            severity: IssueSeverity::Warning,
            message: format!(
                "工作目录在会话中变化了 {cwd_changes} 次。如果 cwd 信息被注入到 system \
                 prompt 中，每次变化都会导致 cache miss。"
            ),
            affected_calls: cwd_changes + 1,
        });
        suggested_fixes.push(
            "将 cwd/workspace 信息从 system prompt 移到第一条 user message 中，\
             保持 system prefix 在不同目录间不变。"
                .to_string(),
        );
    }

    // Subagent prompt variation
    let worker_variation = worker_hashes.len().saturating_sub(1);
    if worker_variation > 0 && calls_with_worker_prompt > 1 {
        issues.push(CacheIssue {
            kind: CacheIssueKind::WorkerPromptVariation,
            severity: IssueSeverity::Info,
            message: format!(
                "Subagent prompt 在 {calls_with_worker_prompt} 次调用中出现了 \
                 {worker_variation} 种变化。不同的 subagent prompt 无法共享 cache。"
            ),
            affected_calls: calls_with_worker_prompt,
        });
    }

    // Multi-model
    if models.len() > 1 {
        issues.push(CacheIssue {
            kind: CacheIssueKind::MultiModelNoSharedCache,
            severity: IssueSeverity::Info,
            message: format!(
                "时间窗口内使用了 {} 个不同模型：{:?}。不同模型的 cache 完全不互通。",
                models.len(),
                models.iter().collect::<Vec<_>>()
            ),
            affected_calls: total_llm_calls,
        });
    }

    // Near-zero cached tokens
    if near_zero_cached > 0 {
        let ratio = near_zero_cached as f64 / total_llm_calls.max(1) as f64;
        let severity = if ratio > 0.8 {
            IssueSeverity::Critical
        } else if ratio > 0.3 {
            IssueSeverity::Warning
        } else {
            IssueSeverity::Info
        };
        issues.push(CacheIssue {
            kind: CacheIssueKind::NearZeroCachedTokens,
            severity,
            message: format!(
                "{near_zero_cached}/{total_llm_calls} 次调用的 cache 命中 token 接近 0。\
                 即使 provider 支持 prompt caching（如 DeepSeek KVCache），cache 可能未生效。"
            ),
            affected_calls: near_zero_cached,
        });
        suggested_fixes.push(
            "确认 provider 是否支持 prompt caching。DeepSeek 在相同 user_id + \
             相同 system prefix 下会自动启用 KVCache。检查 user_id 和 system prefix 是否一致。"
                .to_string(),
        );
    }

    // Cache write higher than read
    if total_cache_creation_input_tokens > total_cached_input_tokens
        && total_cache_creation_input_tokens > 0
    {
        issues.push(CacheIssue {
            kind: CacheIssueKind::CacheWriteHigherThanRead,
            severity: IssueSeverity::Info,
            message: "cache write 高于 cache read，说明 cache 主要在被创建而非被复用。\
                 连续发送相似请求后 read rate 应上升；如果持续低则需要检查前缀稳定性。"
                .to_string(),
            affected_calls: calls_with_cache_write,
        });
    }

    // ── Default message when nothing obviously wrong ────────────────────
    if issues.is_empty() && total_llm_calls > 0 {
        issues.push(CacheIssue {
            kind: CacheIssueKind::MissingUsageData,
            severity: IssueSeverity::Info,
            message: "当前数据未发现明显的 cache 问题。继续观察相同模型下的 read rate 趋势。"
                .to_string(),
            affected_calls: 0,
        });
    }
    // If the only issue is the "no issues" placeholder, don't show it as a real problem.
    // (Kept for backward compat — the frontend filters on severity.)
    if issues.len() == 1 && issues[0].affected_calls == 0 {
        issues.clear();
    }

    if suggested_fixes.is_empty() && total_llm_calls > 0 {
        suggested_fixes
            .push("暂无自动生成的修复建议。cache 表现正常或数据不足以诊断。".to_string());
    }

    let fingerprint_changes = CacheFingerprintChanges {
        system_prompt_hash_changes: system_hashes.len().saturating_sub(1),
        tools_schema_hash_changes: tools_hashes.len().saturating_sub(1),
        cwd_hash_changes: cwd_hashes.len().saturating_sub(1),
        worker_prompt_hash_changes: worker_hashes.len().saturating_sub(1),
        distinct_providers: models.len(), // proxy: each model implies a provider
    };

    CacheDiagnostics {
        overall_read_rate,
        total_input_tokens,
        total_cached_input_tokens,
        total_cache_creation_input_tokens,
        total_llm_calls,
        calls_missing_usage,
        distinct_models: models.len(),
        issues,
        suggested_fixes,
        fingerprint_changes,
    }
}
