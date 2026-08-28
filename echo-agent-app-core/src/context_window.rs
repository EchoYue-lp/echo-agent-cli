//! 当前会话上下文窗口占用快照与渲染辅助（应用层 UI 投影）。
//!
//! 语义对齐 Claude Code statusline 的 context_window.used_percentage：
//! 数据源 = 最近一次 LLM 响应的真实 prompt_tokens（含 cache），不是累计总量。
//! 这是"当前上下文长度"，每次 LLM 调用后覆盖。

use std::time::Instant;

/// 当前会话上下文窗口占用快照（来自最近一次 LLM 响应）。
#[derive(Clone, Debug, Default)]
pub struct ContextWindowSnapshot {
    /// 本次请求的实际输入 token（= 当前上下文主体），已含 cache 部分。
    pub input_tokens: u32,
    /// 其中命中缓存的部分（展示时可单独标注 cached）。
    pub cached_tokens: u32,
    /// 写入缓存的部分（参考）。
    pub cache_creation_tokens: u32,
    /// 本次生成 token（不计入"占用"，仅参考）。
    pub output_tokens: u32,
    /// 模型上下文窗口上限（来自 agent token_limit；0 表示未知）。
    pub context_window_size: u32,
    /// 首次响应前为 None → UI 显示占位。
    pub updated_at: Option<Instant>,
}

impl ContextWindowSnapshot {
    /// 占用百分比 = input_tokens / context_window_size × 100。
    /// window_size 为 0（未知）时返回 None，UI 不显示百分比。
    pub fn used_percentage(&self) -> Option<u16> {
        if self.context_window_size == 0 {
            return None;
        }
        // u32 运算用 u64 中间值防溢出，clamp 到 [0,100]。
        let pct = (self.input_tokens as u64) * 100 / (self.context_window_size as u64);
        Some(pct.clamp(0, 100) as u16)
    }

    /// 是否已有有效快照（首次响应前 / 刚压缩后为 false → UI 显示占位）。
    pub fn is_available(&self) -> bool {
        self.updated_at.is_some()
    }

    /// 压缩边界 / 会话边界：置为 unavailable。
    ///
    /// 圆环回到「首条响应前」占位（`--` / `○`），等下一轮 LlmUsage 再填。
    /// 保留 `context_window_size`（模型上限不变）。
    pub fn clear_usage(&mut self) {
        self.input_tokens = 0;
        self.cached_tokens = 0;
        self.cache_creation_tokens = 0;
        self.output_tokens = 0;
        self.updated_at = None;
    }
}

/// 当前会话的 LLM 用量累计统计（用于缓存命中率等会话级指标）。
///
/// 与 [`ContextWindowSnapshot`]（瞬时占用）的区别：本结构是累计式，
/// 每次 LlmUsage 累加；范围 = 当前 conversation。
/// 压缩不重置本结构（会话级成本指标跨压缩保留）；
/// 仅在 /clear、新会话、clearMessages、replaceMessages 时清零。
#[derive(Clone, Debug, Default)]
pub struct ContextUsageAccumulator {
    /// 累计输入 token（所有 usage_reported=true 的响应之和）。
    pub total_input: u64,
    /// 累计命中缓存的 token。
    pub total_cached: u64,
}

impl ContextUsageAccumulator {
    /// 累加一次 LLM 响应的用量。仅当 usage_reported=true 时累加，
    /// 避免 provider 未报 usage 时（cached/input 可能为 0）污染命中率。
    pub fn record(&mut self, input: u64, cached: u64, usage_reported: bool) {
        if !usage_reported {
            return;
        }
        self.total_input = self.total_input.saturating_add(input);
        self.total_cached = self.total_cached.saturating_add(cached);
    }

    /// 会话平均缓存命中率 = total_cached / total_input。
    /// total_input=0 时返回 None（首条响应前，或会话刚重置）。
    pub fn cache_hit_rate(&self) -> Option<f64> {
        if self.total_input == 0 {
            return None;
        }
        Some(self.total_cached as f64 / self.total_input as f64)
    }

    /// 会话边界重置（/clear、新会话等）。压缩路径禁止调用。
    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

/// 把 token 数格式化为人类可读：≥1000 用 k 单位（四舍五入到 1 位小数，但整 k 时省略小数）。
/// 例：0 → "0"，999 → "999"，1500 → "1.5k"，1999 → "2k"，128000 → "128k"，128500 → "128.5k"。
pub fn format_token_count(n: u32) -> String {
    if n < 1000 {
        n.to_string()
    } else {
        // 固定格式化为 1 位小数，再去掉末尾的 ".0"（128.0k → 128k，但保留 1.5k）。
        let formatted = format!("{:.1}k", n as f64 / 1000.0);
        if let Some(stripped) = formatted.strip_suffix(".0k") {
            format!("{}k", stripped)
        } else {
            formatted
        }
    }
}

/// 生成 10 格 ASCII 进度条：▓ 已用 / ░ 剩余。
/// pct 为 None（window 未知）时返回空串。
///
/// v2 圆环指示器优先用 [`render_ring_char`]；本函数保留给兼容/测试。
pub fn render_progress_bar(used_percentage: Option<u16>) -> String {
    let pct = match used_percentage {
        Some(p) => p,
        None => return String::new(),
    };
    // 10 格，filled = ceil(pct/10)；用整数 div_ceil 避免浮点。
    let filled = (pct as u32).div_ceil(10);
    let filled = filled.clamp(0, 10) as usize;
    let bar: String = "▓".repeat(filled);
    let rest: String = "░".repeat(10 - filled);
    format!("{}{}", bar, rest)
}

/// 用 unicode 圆字符近似环形进度（5 档，离散近似）。
/// None → '○'（空环：首条响应前，或刚压缩后）。
/// 这是 TUI 受 cell-grid 限制的近似做法（Claude Code 同款）。
pub fn render_ring_char(used_percentage: Option<u16>) -> char {
    match used_percentage {
        None | Some(0) => '○',
        Some(1..=25) => '◔',
        Some(26..=50) => '◑',
        Some(51..=75) => '◓',
        Some(_) => '●',
    }
}

/// 根据占用百分比返回颜色分级：绿(<70) / 黄(70-89) / 红(≥90)。
/// 返回语义标签，由调用方映射到具体颜色（TUI theme 色 / Web CSS 变量）。
pub fn usage_tier(used_percentage: Option<u16>) -> UsageTier {
    match used_percentage {
        None => UsageTier::Unknown,
        Some(p) if p >= 90 => UsageTier::Critical,
        Some(p) if p >= 70 => UsageTier::High,
        Some(_) => UsageTier::Normal,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UsageTier {
    Normal,
    High,
    Critical,
    Unknown,
}

#[cfg(test)]
mod tests {
    use super::*;

    mod used_percentage {
        use super::*;

        #[test]
        fn zero_usage() {
            let s = ContextWindowSnapshot {
                input_tokens: 0,
                context_window_size: 128_000,
                updated_at: Some(Instant::now()),
                ..Default::default()
            };
            assert_eq!(s.used_percentage(), Some(0));
        }

        #[test]
        fn half_usage() {
            let s = ContextWindowSnapshot {
                input_tokens: 64_000,
                context_window_size: 128_000,
                updated_at: Some(Instant::now()),
                ..Default::default()
            };
            assert_eq!(s.used_percentage(), Some(50));
        }

        #[test]
        fn full_usage() {
            let s = ContextWindowSnapshot {
                input_tokens: 128_000,
                context_window_size: 128_000,
                updated_at: Some(Instant::now()),
                ..Default::default()
            };
            assert_eq!(s.used_percentage(), Some(100));
        }

        #[test]
        fn over_capacity_clamps_to_100() {
            let s = ContextWindowSnapshot {
                input_tokens: 200_000,
                context_window_size: 128_000,
                updated_at: Some(Instant::now()),
                ..Default::default()
            };
            assert_eq!(s.used_percentage(), Some(100));
        }

        #[test]
        fn unknown_window_returns_none() {
            let s = ContextWindowSnapshot {
                input_tokens: 15_000,
                context_window_size: 0,
                updated_at: Some(Instant::now()),
                ..Default::default()
            };
            assert_eq!(s.used_percentage(), None);
        }

        #[test]
        fn fresh_snapshot_is_unavailable() {
            let s = ContextWindowSnapshot::default();
            assert!(!s.is_available());
            assert_eq!(s.updated_at, None);
        }
    }

    mod format_token_count {
        use super::*;

        #[test]
        fn zero() {
            assert_eq!(format_token_count(0), "0");
        }

        #[test]
        fn under_thousand_stays_raw() {
            assert_eq!(format_token_count(999), "999");
        }

        #[test]
        fn exact_thousand_no_decimal() {
            assert_eq!(format_token_count(1000), "1k");
        }

        #[test]
        fn with_decimal() {
            assert_eq!(format_token_count(1500), "1.5k");
        }

        #[test]
        fn near_thousand_strips_decimal() {
            // 四舍五入到整 k 时省略小数：1999 → "2k"，而非 "2.0k"。
            assert_eq!(format_token_count(1999), "2k");
        }

        #[test]
        fn large_exact() {
            assert_eq!(format_token_count(128_000), "128k");
        }

        #[test]
        fn large_with_decimal() {
            assert_eq!(format_token_count(128_500), "128.5k");
        }
    }

    mod render_progress_bar {
        use super::*;

        #[test]
        fn none_returns_empty() {
            assert_eq!(render_progress_bar(None), "");
        }

        #[test]
        fn zero_is_all_empty() {
            assert_eq!(render_progress_bar(Some(0)), "░░░░░░░░░░");
        }

        #[test]
        fn five_percent_one_filled() {
            assert_eq!(render_progress_bar(Some(5)), "▓░░░░░░░░░");
        }

        #[test]
        fn fifty_percent_five_filled() {
            assert_eq!(render_progress_bar(Some(50)), "▓▓▓▓▓░░░░░");
        }

        #[test]
        fn ninety_five_percent_ten_filled() {
            assert_eq!(render_progress_bar(Some(95)), "▓▓▓▓▓▓▓▓▓▓");
        }

        #[test]
        fn full_is_all_filled() {
            assert_eq!(render_progress_bar(Some(100)), "▓▓▓▓▓▓▓▓▓▓");
        }
    }

    mod usage_tier {
        use super::*;

        #[test]
        fn none_is_unknown() {
            assert_eq!(usage_tier(None), UsageTier::Unknown);
        }

        #[test]
        fn under_70_is_normal() {
            assert_eq!(usage_tier(Some(0)), UsageTier::Normal);
            assert_eq!(usage_tier(Some(69)), UsageTier::Normal);
        }

        #[test]
        fn seventy_to_89_is_high() {
            assert_eq!(usage_tier(Some(70)), UsageTier::High);
            assert_eq!(usage_tier(Some(89)), UsageTier::High);
        }

        #[test]
        fn ninety_plus_is_critical() {
            assert_eq!(usage_tier(Some(90)), UsageTier::Critical);
            assert_eq!(usage_tier(Some(100)), UsageTier::Critical);
        }
    }

    mod clear_usage {
        use super::*;

        #[test]
        fn clears_tokens_keeps_window_size() {
            let mut s = ContextWindowSnapshot {
                input_tokens: 50_000,
                cached_tokens: 40_000,
                cache_creation_tokens: 1_000,
                output_tokens: 200,
                context_window_size: 128_000,
                updated_at: Some(Instant::now()),
            };
            s.clear_usage();
            assert!(!s.is_available());
            assert_eq!(s.input_tokens, 0);
            assert_eq!(s.cached_tokens, 0);
            assert_eq!(s.cache_creation_tokens, 0);
            assert_eq!(s.output_tokens, 0);
            assert_eq!(s.context_window_size, 128_000);
            assert_eq!(s.updated_at, None);
        }
    }

    mod usage_accumulator {
        use super::*;

        #[test]
        fn record_accumulates_when_reported() -> anyhow::Result<()> {
            let mut a = ContextUsageAccumulator::default();
            a.record(1000, 800, true);
            a.record(500, 400, true);
            assert_eq!(a.total_input, 1500);
            assert_eq!(a.total_cached, 1200);
            let rate = a.cache_hit_rate().ok_or_else(|| anyhow::anyhow!("rate"))?;
            assert!((rate - 0.8).abs() < 1e-9);
            Ok(())
        }

        #[test]
        fn record_skips_when_not_reported() {
            let mut a = ContextUsageAccumulator::default();
            a.record(1000, 800, true);
            a.record(9999, 9999, false);
            assert_eq!(a.total_input, 1000);
            assert_eq!(a.total_cached, 800);
        }

        #[test]
        fn cache_hit_rate_none_when_empty() {
            let a = ContextUsageAccumulator::default();
            assert_eq!(a.cache_hit_rate(), None);
        }

        #[test]
        fn cache_hit_rate_zero_and_full() {
            let mut a = ContextUsageAccumulator::default();
            a.record(100, 0, true);
            assert_eq!(a.cache_hit_rate(), Some(0.0));
            a.reset();
            a.record(100, 100, true);
            assert_eq!(a.cache_hit_rate(), Some(1.0));
        }

        #[test]
        fn saturating_add_does_not_panic() {
            let mut a = ContextUsageAccumulator {
                total_input: u64::MAX,
                total_cached: u64::MAX,
            };
            a.record(1, 1, true);
            assert_eq!(a.total_input, u64::MAX);
            assert_eq!(a.total_cached, u64::MAX);
        }

        #[test]
        fn reset_clears() {
            let mut a = ContextUsageAccumulator::default();
            a.record(100, 50, true);
            a.reset();
            assert_eq!(a.total_input, 0);
            assert_eq!(a.total_cached, 0);
            assert_eq!(a.cache_hit_rate(), None);
        }

        #[test]
        fn compress_boundary_keeps_accumulator() -> anyhow::Result<()> {
            // 压缩只清 Snapshot，不清 Accumulator（设计契约）。
            let mut snap = ContextWindowSnapshot {
                input_tokens: 80_000,
                context_window_size: 128_000,
                updated_at: Some(Instant::now()),
                ..Default::default()
            };
            let mut acc = ContextUsageAccumulator::default();
            acc.record(80_000, 70_000, true);
            snap.clear_usage();
            assert!(!snap.is_available());
            assert_eq!(acc.total_input, 80_000);
            let rate = acc
                .cache_hit_rate()
                .ok_or_else(|| anyhow::anyhow!("rate"))?;
            assert!((rate - 0.875).abs() < 1e-9);
            Ok(())
        }
    }

    mod render_ring_char_tests {
        use super::*;

        #[test]
        fn none_and_zero_are_empty_ring() {
            assert_eq!(render_ring_char(None), '○');
            assert_eq!(render_ring_char(Some(0)), '○');
        }

        #[test]
        fn quarter_half_three_quarter_full() {
            assert_eq!(render_ring_char(Some(1)), '◔');
            assert_eq!(render_ring_char(Some(25)), '◔');
            assert_eq!(render_ring_char(Some(26)), '◑');
            assert_eq!(render_ring_char(Some(50)), '◑');
            assert_eq!(render_ring_char(Some(51)), '◓');
            assert_eq!(render_ring_char(Some(75)), '◓');
            assert_eq!(render_ring_char(Some(76)), '●');
            assert_eq!(render_ring_char(Some(100)), '●');
        }
    }
}
