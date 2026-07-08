# 上下文窗口占用指示器 实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 在 EKO 的 TUI status bar 和 Web ChatInput footer 增加始终可见的上下文窗口占用指示器（百分比 + 进度条 + 颜色分级），数据源用最近一次 LLM 响应的真实 `prompt_tokens`。

**Architecture:** 纯应用层（`echo-agent-cli`），零框架改动。数据源 `AgentEvent::LlmUsage` 框架已发射；TUI 当前丢弃它（改为接住），Web 前端当前 no-op（改为接住）。两路径各自维持一个 `ContextWindowSnapshot`，按统一规格渲染。顺带修复 TUI status bar 现有 bug（显示 request_count/1000 而非真实 token）。

**Tech Stack:** Rust（ratatui TUI + Tauri 后端）、TypeScript/React/Zustand（Web 前端）、Tailwind v4。

**Spec:** `docs/2026-07-08-context-window-indicator-design.md`

---

## 关键事实（实现前必读）

1. **`app.tokens` 注释与实现不符**：`src/tui/mod.rs:188` 注释写 `(prompt, completion, total)`，但 `events.rs:291` 实际写的是 `.2 += 1`（request count）。本计划**修正**为真实语义，且新增独立的 `ContextWindowSnapshot` 字段。
2. **Web 事件流已通**：后端 `chat.rs:1070` 已把 `LlmUsage` 映射成 `ChatEvent::LlmUsage` 发到前端；前端 `chatEventHandler.ts:60` 只是 no-op。Web 路径只改前端。
3. **`ConfiguredModel.context_window` 前端已有**（`api.ts:643`）：Web 路径的 window_size 直接从当前默认模型取，**后端 ChatEvent 无需加字段**。
4. **TUI 的 window_size**：TUI 无模型配置 API，通过 `agent.config().get_token_limit()` 在 TUI 启动时读一次存入 `TuiApp`（GUI 的 `panels.rs:1015` 已用同样方式）。
5. **`ContextWindowSnapshot` 是 UI 投影**，纯应用层概念，放 `echo-agent-app-core`（TUI 和 GUI 的共同依赖）。

---

## 文件结构

### 新建
- `echo-agent-app-core/src/context_window.rs` — `ContextWindowSnapshot` 结构 + 百分比/格式化/进度条计算（纯逻辑，无 IO，可单测）

### 修改
- `echo-agent-app-core/src/lib.rs` — 声明 `pub mod context_window;`
- `src/tui/mod.rs` — `TuiApp` 新增 `context_window_size: u32` + `context_snapshot: ContextWindowSnapshot` 字段；`TuiApp::new` 签名加参数；启动处传入 token_limit；修正 `tokens` 注释
- `src/tui/events.rs` — `LlmUsage` 分支从"丢弃"改为"写入 snapshot"
- `src/tui/widgets/status_bar.rs` — 用 snapshot 渲染进度条 + 百分比 + 颜色分级
- `web-frontend/src/stores/chatStore.ts` — 新增 `contextWindow` state 字段 + `setContextWindow` action
- `web-frontend/src/hooks/chatEventHandler.ts` — `llm_usage` 分支从 no-op 改为写 store
- `web-frontend/src/components/chat/ChatInput.tsx` — footer 渲染上下文指示器组件

---

## Task 1: ContextWindowSnapshot 核心逻辑（TDD）

**Files:**
- Create: `echo-agent-cli/echo-agent-app-core/src/context_window.rs`
- Modify: `echo-agent-cli/echo-agent-app-core/src/lib.rs`

这是纯逻辑模块（百分比、token 数字格式化、进度条生成），无任何 IO，最适合 TDD。

- [ ] **Step 1: 写失败测试（百分比 + 格式化 + 进度条）**

创建 `echo-agent-app-core/src/context_window.rs`，先只放测试：

```rust
//! 当前会话上下文窗口占用快照与渲染辅助（应用层 UI 投影）。
//!
//! 语义对齐 Claude Code statusline 的 context_window.used_percentage：
//! 数据源 = 最近一次 LLM 响应的真实 prompt_tokens（含 cache），不是累计总量。
//! 这是"当前上下文长度"，每次 LLM 调用后覆盖。

use std::time::Instant;

/// 当前会话上下文窗口占用快照（来自最近一次 LLM 响应）。
#[derive(Clone, Debug)]
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

impl Default for ContextWindowSnapshot {
    fn default() -> Self {
        Self {
            input_tokens: 0,
            cached_tokens: 0,
            cache_creation_tokens: 0,
            output_tokens: 0,
            context_window_size: 0,
            updated_at: None,
        }
    }
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

    /// 是否已有有效快照（首次响应前为 false → UI 显示占位）。
    pub fn is_available(&self) -> bool {
        self.updated_at.is_some()
    }
}

/// 把 token 数格式化为人类可读：≥1000 用 k 单位（保留 1 位小数），否则原数。
/// 例：0 → "0"，999 → "999"，1500 → "1.5k"，128000 → "128k"。
pub fn format_token_count(n: u32) -> String {
    if n < 1000 {
        n.to_string()
    } else {
        // 除以 100，再保留 1 位小数：128000/100=1280 → "12.8k"... 不对。
        // 正确做法：除以 1000 得到 k 值，用整数与一位小数。
        let k = n as f64 / 1000.0;
        // 整除时省略小数（128k 而非 128.0k），否则保留 1 位（1.5k）。
        if (k - k.round()).abs() < f64::EPSILON {
            format!("{}k", k as u32)
        } else {
            format!("{:.1}k", k)
        }
    }
}

/// 生成 10 格 ASCII 进度条：▓ 已用 / ░ 剩余。
/// pct 为 None（window 未知）时返回空串。
pub fn render_progress_bar(used_percentage: Option<u16>) -> String {
    let pct = match used_percentage {
        Some(p) => p,
        None => return String::new(),
    };
    // 10 格，filled = ceil(pct/10)；用整数运算避免浮点。
    let filled = ((pct as u32) + 9) / 10; // ceil(pct/10)
    let filled = filled.clamp(0, 10) as usize;
    let bar: String = "▓".repeat(filled);
    let rest: String = "░".repeat(10 - filled);
    format!("{}{}", bar, rest)
}

/// 根据占用百分比返回颜色分级：绿(<70) / 黄(70-89) / 红(≥90)。
/// 返回语义标签，由调用方映射到具体颜色（TUI theme 色 / Web CSS 变量）。
pub fn usage_tier(used_percentage: Option<u16>) -> UsageTier {
    match used_percentage {
        None => UsageTier::Unknown,
        Some(p) if p >= 90 => UsageTier::Critical,
        Some(p) if p >= 70 => UsageTier::High,
        Some(p) => UsageTier::Normal,
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
}
```

- [ ] **Step 2: 在 lib.rs 声明模块**

编辑 `echo-agent-app-core/src/lib.rs`，在现有 `pub mod` 列表里加一行（与其它 `pub mod` 同样位置，按字母序或紧跟现有模块）。

先查看 lib.rs 现有 mod 声明：

Run: `grep -n "pub mod" echo-agent-app-core/src/lib.rs`

在合适位置加：

```rust
pub mod context_window;
```

- [ ] **Step 3: 运行测试，验证通过**

Run:
```bash
cd /Users/ls/MyWork/code/ylp_agent_learn/lp-agent/echo-agent-cli
cargo test -p echo-agent-app-core context_window
```

Expected: PASS（所有 used_percentage / format_token_count / render_progress_bar / usage_tier 测试通过）

- [ ] **Step 4: 编译检查**

Run:
```bash
cd /Users/ls/MyWork/code/ylp_agent_learn/lp-agent/echo-agent-cli
cargo check -p echo-agent-app-core
```

Expected: 零错误

- [ ] **Step 5: 提交**

```bash
cd /Users/ls/MyWork/code/ylp_agent_learn/lp-agent/echo-agent-cli
cargo fmt --all
git add echo-agent-app-core/src/context_window.rs echo-agent-app-core/src/lib.rs
git -c commit.gpgsign=false commit -m "feat(app-core): ContextWindowSnapshot 上下文占用快照与渲染辅助

对齐 Claude Code statusline:用真实 prompt_tokens 计算占用百分比。
纯逻辑模块(百分比/格式化/进度条/颜色分级),无 IO,TDD。"
```

---

## Task 2: TUI — TuiApp 持有 snapshot 与 window_size

**Files:**
- Modify: `src/tui/mod.rs`（struct 字段 `:188`、`TuiApp::new` `:323`、构造点 `:345`、启动点 `:1165`）

- [ ] **Step 1: 加 import 与字段**

在 `src/tui/mod.rs` 顶部 import 区加：

```rust
use echo_agent_app_core::context_window::ContextWindowSnapshot;
```

在 `TuiApp` struct（`mod.rs:154`）里，紧跟现有 `pub tokens: (u32, u32, u32),`（`:188`）后加两个字段：

```rust
    /// Token usage 累计 (prompt, completion, request_count)。
    /// 注意：prompt/completion 是累计历史值；request_count 用于统计调用次数。
    /// "当前上下文占用"由 context_snapshot 单独维持，见下。
    pub tokens: (u32, u32, u32),
    /// 模型上下文窗口上限（启动时从 agent token_limit 读一次；0 表示未知）。
    pub context_window_size: u32,
    /// 当前上下文窗口占用快照（每次 LlmUsage 后覆盖）。
    pub context_snapshot: ContextWindowSnapshot,
```

（同时修正了原 `tokens` 注释的语义错误。）

- [ ] **Step 2: 在构造体里初始化字段**

在 `TuiApp::new`（`mod.rs:323`）的 `Self { ... }` 里，紧跟 `tokens: (0, 0, 0),`（`:345`）后加：

```rust
            tokens: (0, 0, 0),
            context_window_size: 0,
            context_snapshot: ContextWindowSnapshot::default(),
```

- [ ] **Step 3: 编译（此时会因 `:1165` 调用处未传 window_size 而报错，预期）**

Run:
```bash
cd /Users/ls/MyWork/code/ylp_agent_learn/lp-agent/echo-agent-cli
cargo check -p echo-agent-cli --no-default-features --features tui 2>&1 | head -30
```

Expected: 编译通过（字段有默认初始化，`TuiApp::new` 签名未变，`context_window_size` 初始为 0；Step 4 再填真实值）。若 feature 名不是 `tui`，先查：

Run: `grep -n "features" Cargo.toml | head` 确认 TUI 的 feature 名。

> 注意：若 `cargo check` 报找不到 `ContextWindowSnapshot`，确认 `echo-agent-app-core` 在 `echo-agent-cli` 的依赖里（应已在），且 lib.rs 已声明 `pub mod context_window`。

- [ ] **Step 4: 启动时填入真实 window_size**

在 `src/tui/mod.rs` 启动处（`:1156-1165` 附近，已有 `let model = agent.read(...)` 读模型名）。在 `let mut app = TuiApp::new(...)`（`:1165`）**之后**加：

```rust
    let mut app = TuiApp::new(model, mode, theme);
    // 读取当前模型的上下文窗口上限（与 GUI panels.rs 同样走 agent.config().get_token_limit()）。
    app.context_window_size = agent
        .read(|a| {
            use echo_agent::agent::Agent;
            a.config().get_token_limit() as u32
        })
        .await;
    app.context_snapshot.context_window_size = app.context_window_size;
```

> 说明：`agent.read` 在同一作用域已用过（`:1157`），闭包签名一致。`get_token_limit()` 返回 `usize`，转 `u32`（context window 不会超 u32 范围）。

- [ ] **Step 5: 编译检查**

Run:
```bash
cd /Users/ls/MyWork/code/ylp_agent_learn/lp-agent/echo-agent-cli
cargo check --no-default-features --features tui
```

Expected: 零错误

> 若 `get_token_limit()` 方法名/签名与预期不符，查 `echo-agent/src/agent/mod.rs` 或 `echo-core` 的 `AgentConfig`：`grep -rn "get_token_limit\|fn token_limit" echo-agent/src echo-agent/echo-core/src`，用实际方法名。

- [ ] **Step 6: 暂不提交**（Task 3 一起提交，因为此时 snapshot 还没被写入，功能未闭环）

---

## Task 3: TUI — 接住 LlmUsage 写入 snapshot

**Files:**
- Modify: `src/tui/events.rs`（`LlmUsage` 分支 `:849-867`）

- [ ] **Step 1: 改写 LlmUsage 分支**

当前 `src/tui/events.rs:849-867` 是丢弃逻辑（只打 debug 日志后 `return true`）。改为：打日志 + 写 snapshot + 返回 true（仍不作为聊天消息渲染，但 snapshot 更新让 status bar 能显示）。

定位现有代码块（`events.rs:848-867`）：

```rust
            echo_agent::agent::AgentEvent::LlmUsage {
                prompt_tokens,
                cached_prompt_tokens,
                cache_creation_prompt_tokens,
                usage_reported,
                ..
            } => {
                tracing::debug!(
                    prompt_tokens,
                    cached_prompt_tokens,
                    cache_creation_prompt_tokens,
                    usage_reported,
                    "TUI: LLM usage reported (cache stats; not rendered)"
                );
                return true;
            }
```

**问题**：这段在 `map_event` 类的纯映射函数里（返回 `bool` 表示是否继续），无法直接访问 `&mut TuiApp`。需要确认这个函数签名与调用点。

先查函数签名与调用点：

Run:
```bash
cd /Users/ls/MyWork/code/ylp_agent_learn/lp-agent/echo-agent-cli
grep -n "fn.*map\|LlmUsage\|return true" src/tui/events.rs | head
```

确认：`events.rs` 里这段在哪个函数内、能否拿到 `&mut app`。

> **关键判断**：从 `events.rs:876` 的 `self.tx.send(mapped)` 看，这是 `TuiChatSink`（或类似 sink）的 `on_agent_event` 方法，它**只有 `&self`（持有 sender），没有 `&mut TuiApp`**。app 的更新在主循环的 `while let Ok(event) = agent_rx.try_recv()`（`:275`）里做，那里**才有 `&mut app`**。
>
> 因此 snapshot 写入**不能放在 sink 里**。正确做法：在 `events.rs` 的映射函数里，把 `LlmUsage` 映射成一个**本地 `AgentEvent` 变体**（让主循环收到），在主循环 `match` 里写 snapshot。
>
> 但 `LlmUsage` 当前是 `return true`（丢弃，不发给主循环）。最简单的改法：让 `LlmUsage` **透传**给主循环（像其它事件一样 `=> AgentEvent::LlmUsage {...}`），然后在主循环 match 里处理。

**先在 TUI 本地 AgentEvent enum 加 LlmUsage 变体**（`events.rs:208-237`，enum 定义）。在 `ContextCompressed { ... }`（`:231-236`）之后加：

```rust
    /// Provider-reported LLM usage（透传框架事件，用于上下文窗口占用展示）。
    LlmUsage {
        prompt_tokens: usize,
        completion_tokens: usize,
        cached_prompt_tokens: usize,
        cache_creation_prompt_tokens: usize,
    },
```

> 字段类型用 `usize`（与框架 `AgentEvent::LlmUsage` 的 `echo-core/src/agent/mod.rs:52-67` 完全对齐，都是 `usize`，无需转换）。只保留 snapshot 需要的 4 个字段（省掉 model/total_tokens/usage_reported）。

**改为透传**（替换 `events.rs:848-867` 整个 `LlmUsage` 分支）：

```rust
            echo_agent::agent::AgentEvent::LlmUsage {
                prompt_tokens,
                completion_tokens,
                cached_prompt_tokens,
                cache_creation_prompt_tokens,
                ..
            } => {
                // 透传给主循环：snapshot 更新需要 &mut TuiApp，主循环才拿得到。
                // （sink 这里只有 &self，无法更新 app 状态。）
                tracing::debug!(
                    prompt_tokens,
                    cached_prompt_tokens,
                    cache_creation_prompt_tokens,
                    "TUI: LLM usage reported — forwarding to main loop for context snapshot"
                );
                AgentEvent::LlmUsage {
                    prompt_tokens,
                    completion_tokens,
                    cached_prompt_tokens,
                    cache_creation_prompt_tokens,
                }
            }
```

- [ ] **Step 2: 在主循环 match 里写 snapshot**

在 `src/tui/mod.rs` 主循环 `while let Ok(event) = agent_rx.try_recv()`（`:275`）的 `match event` 里，加一个 `LlmUsage` 分支（放在 `ThinkEnd` 分支 `:284-292` 之后）：

```rust
                AgentEvent::LlmUsage {
                    prompt_tokens,
                    completion_tokens,
                    cached_prompt_tokens,
                    cache_creation_prompt_tokens,
                } => {
                    // 更新"当前上下文占用"快照（覆盖式，对齐 Claude Code 语义）。
                    // prompt_tokens 是本次请求的真实输入 token（已含 cache 部分）。
                    app.context_snapshot = ContextWindowSnapshot {
                        input_tokens: prompt_tokens as u32,
                        cached_tokens: cached_prompt_tokens as u32,
                        cache_creation_tokens: cache_creation_prompt_tokens as u32,
                        output_tokens: completion_tokens as u32,
                        context_window_size: app.context_window_size,
                        updated_at: Some(std::time::Instant::now()),
                    };
                }
```

> 字段名与 Task 3 Step 1 新增的本地 `AgentEvent::LlmUsage` 变体一致（全是 `usize`）。`as u32` 转换是安全的——context window token 数远在 u32 范围内。

- [ ] **Step 3: 编译检查**

Run:
```bash
cd /Users/ls/MyWork/code/ylp_agent_learn/lp-agent/echo-agent-cli
cargo check --no-default-features --features tui
```

Expected: 零错误

- [ ] **Step 4: 暂不提交**（Task 4 渲染完成后一起提交，闭环验证）

---

## Task 4: TUI — StatusBar 渲染指示器

**Files:**
- Modify: `src/tui/widgets/status_bar.rs`（`tokens` 渲染 `:32-36`、`line` 构造 `:43-56`）

- [ ] **Step 1: 替换 tokens 渲染段**

当前 `status_bar.rs:32-36`：

```rust
        let tokens = if app.tokens.2 > 0 {
            format!(" · {}k tokens", app.tokens.2 / 1000)
        } else {
            String::new()
        };
```

改为用 snapshot 渲染进度条 + 百分比 + 绝对数。替换为：

```rust
        // 上下文窗口占用（对齐 Claude Code statusline）。
        let ctx = &app.context_snapshot;
        let pct = ctx.used_percentage();
        let bar = echo_agent_app_core::context_window::render_progress_bar(pct);
        let tier = echo_agent_app_core::context_window::usage_tier(pct);
        let ctx_color = match tier {
            echo_agent_app_core::context_window::UsageTier::Critical => t.red,
            echo_agent_app_core::context_window::UsageTier::High => t.yellow,
            _ => t.subtext,
        };
        let context_span = if ctx.is_available() {
            let used_str = echo_agent_app_core::context_window::format_token_count(ctx.input_tokens);
            let win_str = echo_agent_app_core::context_window::format_token_count(ctx.context_window_size);
            match pct {
                Some(p) => format!("  {} {}/{} {}%", bar, used_str, win_str, p),
                None => format!("  {} {}", bar, used_str),
            }
        } else {
            // 首次响应前：显示占位（不显示 0%，避免误导）。
            let win_str = echo_agent_app_core::context_window::format_token_count(ctx.context_window_size);
            format!("  · --/{}", win_str)
        };
```

- [ ] **Step 2: 把 context_span 加进 line 构造**

当前 `status_bar.rs:43-56` 的 `Line::from(vec![...])` 里，把原来的 `Span::styled(tokens, ...)`（`:51`）替换为 `Span::styled(context_span, ...)`：

```rust
        let line = Line::from(vec![
            Span::styled(
                " EKO",
                Style::default().fg(mode_color).add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!("  {}", app.mode), Style::default().fg(t.text)),
            Span::styled(format!("  {}", app.model), Style::default().fg(t.subtext)),
            Span::styled(format!("  {}", state), Style::default().fg(state_color)),
            Span::styled(context_span, Style::default().fg(ctx_color)),
            Span::styled(
                format!("  {}", sidebar_hint),
                Style::default().fg(t.overlay0),
            ),
        ]);
```

> 注意：`tokens` 变量已删（被 `context_span` 取代）。`sidebar_hint` 变量保持不变（`:37-41` 原有）。

- [ ] **Step 3: 确认 theme 有 red/yellow 字段**

Run:
```bash
cd /Users/ls/MyWork/code/ylp_agent_learn/lp-agent/echo-agent-cli
grep -n "pub red\|pub yellow\|pub peach\|pub green" src/tui/theme.rs src/tui/*.rs 2>/dev/null | head
```

确认 `t.red`、`t.yellow` 存在。若颜色字段名不同（如 `t.red` 实为 `t.rose`），用实际字段名。

- [ ] **Step 4: 编译检查**

Run:
```bash
cd /Users/ls/MyWork/code/ylp_agent_learn/lp-agent/echo-agent-cli
cargo check --no-default-features --features tui
```

Expected: 零错误

- [ ] **Step 5: TUI 端到端验证（手动）**

Run:
```bash
cd /Users/ls/MyWork/code/ylp_agent_learn/lp-agent/echo-agent-cli
cargo build --no-default-features --features tui --bin echo-agent-cli 2>&1 | tail -3
```

然后手动启动 TUI，发一条消息，确认 status bar 显示形如 `EKO · coding · gpt-5 · thinking · ▓░░░░░░░░░ 15.2k/128k 12%`（而非旧的 `1k tokens`）。

> 若不便手动验证，至少确认编译通过 + 逻辑正确（snapshot 字段在 events.rs 写入、status_bar.rs 读取，数据流闭环）。

- [ ] **Step 6: 提交（TUI 闭环：Task 2+3+4）**

```bash
cd /Users/ls/MyWork/code/ylp_agent_learn/lp-agent/echo-agent-cli
cargo fmt --all
git add src/tui/mod.rs src/tui/events.rs src/tui/widgets/status_bar.rs
git -c commit.gpgsign=false commit -m "feat(tui): status bar 显示上下文窗口占用指示器

接住 LlmUsage 事件(原被丢弃),写入 ContextWindowSnapshot,
status bar 渲染进度条+百分比+颜色分级(绿/黄/红)。
修正旧 bug:原 tokens 显示 request_count/1000,现为真实 prompt_tokens。
对齐 Claude Code statusline 语义。"
```

---

## Task 5: Web — chatStore 新增 contextWindow state

**Files:**
- Modify: `web-frontend/src/stores/chatStore.ts`

- [ ] **Step 1: 查看现有 ChatState 结构**

Run:
```bash
cd /Users/ls/MyWork/code/ylp_agent_learn/lp-agent/echo-agent-cli
sed -n '1,60p' web-frontend/src/stores/chatStore.ts
```

在 `interface ChatState`（约 `:17`）里找到合适位置加字段。

- [ ] **Step 2: 加 state 字段与 action**

在 `interface ChatState` 里（state 字段区）加：

```typescript
  /** 当前上下文窗口占用（来自最近一次 LLM 响应的真实 prompt_tokens）。 */
  contextWindow: ContextWindowUsage | null;
```

在 action 区加：

```typescript
  /** 更新上下文窗口占用快照（由 llm_usage 事件驱动）。 */
  setContextWindow: (usage: ContextWindowUsage) => void;
  /** 清空上下文窗口占用（会话重置/切换时）。 */
  clearContextWindow: () => void;
```

在文件顶部 import 区加类型定义（若无已存在的）：

```typescript
/** 当前上下文窗口占用快照（对齐 Claude Code statusline 语义）。 */
export interface ContextWindowUsage {
  /** 本次请求的实际输入 token（= 当前上下文主体），已含 cache 部分。 */
  inputTokens: number;
  /** 其中命中缓存的部分。 */
  cachedTokens: number;
  /** 写入缓存的部分。 */
  cacheCreationTokens: number;
  /** 本次生成 token（不计入占用，仅参考）。 */
  outputTokens: number;
  /** 模型上下文窗口上限（来自当前默认模型的 context_window 配置）。 */
  contextWindowSize: number;
}
```

- [ ] **Step 3: 在 create<ChatState>() 实现里初始化与实现 action**

在 store 的 state 初始值里加：

```typescript
  contextWindow: null,
```

在 actions 里加：

```typescript
  setContextWindow: (usage) => set({ contextWindow: usage }),
  clearContextWindow: () => set({ contextWindow: null }),
```

- [ ] **Step 4: 类型检查**

Run:
```bash
cd /Users/ls/MyWork/code/ylp_agent_learn/lp-agent/echo-agent-cli/web-frontend
npx tsc -b 2>&1 | tail -10
```

Expected: 零错误

- [ ] **Step 5: 暂不提交**（Task 6+7 一起提交，Web 闭环）

---

## Task 6: Web — chatEventHandler 接住 llm_usage

**Files:**
- Modify: `web-frontend/src/hooks/chatEventHandler.ts`（`llm_usage` 分支 `:60-64`）

- [ ] **Step 1: 改写 llm_usage 分支**

当前 `chatEventHandler.ts:60-64`：

```typescript
    case 'llm_usage': {
      // Observability-only event. Subagent trace panels consume the same
      // facts; the chat transcript should not render cache telemetry as text.
      break;
    }
```

改为接住事件写 store。注意：window_size 不在事件里（后端 ChatEvent::LlmUsage 不含它），从 store 里已有的当前默认模型配置取——但 chatStore 不持有模型配置。最简方案：让 `setContextWindow` 接收事件里的 token 数，window_size 由调用方（组件）在渲染时结合模型配置计算。

但为保持 store 自洽（snapshot 含 window_size），在此处从 `configuredModels` 不可得（chatEventHandler 不持有它）。**改用方案**：chatStore 的 `contextWindow` 只存 token 数，window_size 由 ChatInput 组件从它自己的 `displayModel.context_window` 取（ChatInput 已加载模型配置，见 `ChatInput.tsx:242` 的 `activeModel`）。

因此 `ContextWindowUsage` 不含 window_size，改为只存 token 数。修正 Task 5 Step 2 的类型定义——删去 `contextWindowSize` 字段：

```typescript
export interface ContextWindowUsage {
  /** 本次请求的实际输入 token（= 当前上下文主体），已含 cache 部分。 */
  inputTokens: number;
  /** 其中命中缓存的部分。 */
  cachedTokens: number;
  /** 写入缓存的部分。 */
  cacheCreationTokens: number;
  /** 本次生成 token（不计入占用，仅参考）。 */
  outputTokens: number;
}
```

（回 Task 5 Step 2 把 `contextWindowSize` 字段删掉，保持一致。）

改写 `llm_usage` 分支：

```typescript
    case 'llm_usage': {
      // 更新上下文窗口占用快照（不作为聊天消息渲染，仅驱动 footer 指示器）。
      // 对齐 Claude Code statusline：用真实 prompt_tokens 表示当前上下文长度。
      store.setContextWindow({
        inputTokens: event.prompt_tokens,
        cachedTokens: event.cached_prompt_tokens,
        cacheCreationTokens: event.cache_creation_prompt_tokens,
        outputTokens: event.completion_tokens,
      });
      break;
    }
```

- [ ] **Step 2: 类型检查**

Run:
```bash
cd /Users/ls/MyWork/code/ylp_agent_learn/lp-agent/echo-agent-cli/web-frontend
npx tsc -b 2>&1 | tail -10
```

Expected: 零错误

- [ ] **Step 3: 暂不提交**（Task 7 一起提交）

---

## Task 7: Web — ChatInput footer 渲染指示器

**Files:**
- Modify: `web-frontend/src/components/chat/ChatInput.tsx`（footer 右侧 `:929-932`）

- [ ] **Step 1: 加 store 订阅与计算**

在 `ChatInput` 组件函数内（与其它 `useState` 同区，约 `:228-246`）加：

```typescript
  // 上下文窗口占用（来自 llm_usage 事件 → chatStore）。
  const contextWindow = useChatStore((s) => s.contextWindow);
  // 当前默认模型的 context_window（已加载的 ConfiguredModel，:242 activeModel）。
  const activeModel = configuredModels.find((model) => model.is_default);
  const contextWindowSize = activeModel?.context_window ?? null;
```

> 若 `activeModel` 已在 `:242` 定义，不要重复定义，复用即可（删掉本处重复，只加 `contextWindowSize`）。

加渲染辅助函数（组件内，在 `return` 之前）：

```typescript
  // 计算上下文占用展示（对齐 Claude Code：真实 prompt_tokens / window_size）。
  const ctxUsage = (() => {
    if (!contextWindow) return null;
    const used = contextWindow.inputTokens;
    const win = contextWindowSize;
    if (win == null || win <= 0) {
      // window 未知：只显示绝对数。
      return { bar: null, pct: null, used, win: null, tier: 'unknown' as const };
    }
    const pct = Math.min(100, Math.round((used / win) * 100));
    const filled = Math.ceil(pct / 10);
    const bar = '▓'.repeat(filled) + '░'.repeat(10 - filled);
    const tier =
      pct >= 90 ? ('critical' as const) : pct >= 70 ? ('high' as const) : ('normal' as const);
    return { bar, pct, used, win, tier };
  })();
```

- [ ] **Step 2: 在 footer 右侧渲染**

当前 `ChatInput.tsx:929-932`：

```tsx
            <div className="flex items-center gap-3">
              <span>Enter 发送</span>
              {text.length > 0 && <span>{text.length} 字</span>}
            </div>
```

改为：

```tsx
            <div className="flex items-center gap-3">
              {ctxUsage && (
                <span
                  className="font-mono text-[11px]"
                  style={{
                    color:
                      ctxUsage.tier === 'critical'
                        ? 'var(--error)'
                        : ctxUsage.tier === 'high'
                          ? 'var(--warning)'
                          : 'var(--text-tertiary)',
                  }}
                  title={
                    ctxUsage.win
                      ? `上下文窗口: ${ctxUsage.used} / ${ctxUsage.win} tokens (${ctxUsage.pct}%)`
                      : `上下文: ${ctxUsage.used} tokens`
                  }
                >
                  {ctxUsage.bar ? (
                    <>
                      <span className="mr-1">{ctxUsage.bar}</span>
                      {formatTokens(ctxUsage.used)}/{formatTokens(ctxUsage.win!)} · {ctxUsage.pct}%
                    </>
                  ) : (
                    <>{formatTokens(ctxUsage.used)} tokens</>
                  )}
                </span>
              )}
              <span>Enter 发送</span>
              {text.length > 0 && <span>{text.length} 字</span>}
            </div>
```

- [ ] **Step 3: 加 formatTokens 辅助函数**

在 `ChatInput.tsx` 文件内（组件外，与其它辅助函数同区，如文件顶部 const 定义区）加：

```typescript
/** token 数格式化：≥1000 用 k 单位（128000 → 128k，1500 → 1.5k）。 */
function formatTokens(n: number): string {
  if (n < 1000) return String(n);
  const k = n / 1000;
  if (Number.isInteger(k)) return `${k}k`;
  return `${k.toFixed(1)}k`;
}
```

- [ ] **Step 4: 确认 CSS 变量存在**

Run:
```bash
cd /Users/ls/MyWork/code/ylp_agent_learn/lp-agent/echo-agent-cli/web-frontend
grep -rn "\-\-error\|\-\-warning\|\-\-text-tertiary" src/ | grep -i "css\|:" | head
```

确认 `--error`、`--warning`、`--text-tertiary` 变量存在。若 error/warning 变量名不同（如 `--danger`、`--warn`），用实际名。

- [ ] **Step 5: 类型检查 + 构建**

Run:
```bash
cd /Users/ls/MyWork/code/ylp_agent_learn/lp-agent/echo-agent-cli/web-frontend
npx tsc -b 2>&1 | tail -10
npm run build 2>&1 | tail -10
```

Expected: 零错误

- [ ] **Step 6: Web 端到端验证（手动，可选）**

启动 GUI（`cargo tauri dev` 或项目既有命令），发消息，确认 ChatInput footer 出现进度条且随对话增长。若不便手动，确认 tsc + build 通过 + 逻辑闭环（事件→store→组件）。

- [ ] **Step 7: 提交（Web 闭环：Task 5+6+7）**

```bash
cd /Users/ls/MyWork/code/ylp_agent_learn/lp-agent/echo-agent-cli/web-frontend
cd ..
git add web-frontend/src/stores/chatStore.ts web-frontend/src/hooks/chatEventHandler.ts web-frontend/src/components/chat/ChatInput.tsx
git -c commit.gpgsign=false commit -m "feat(web): ChatInput footer 显示上下文窗口占用指示器

接住 llm_usage 事件(原 no-op),写入 chatStore.contextWindow,
footer 渲染进度条+百分比+颜色分级。
window_size 取自当前默认模型的 context_window 配置。
对齐 Claude Code statusline 语义。"
```

---

## Task 8: 全量验证与清理

**Files:** 无改动，仅验证

- [ ] **Step 1: Rust 全量验证（根 crate + workspace）**

```bash
cd /Users/ls/MyWork/code/ylp_agent_learn/lp-agent/echo-agent-cli
cargo fmt --all
cargo fmt --all -- --check   # 退出码必须 0
cargo check --workspace
cargo test --workspace
```

Expected: 全部通过

- [ ] **Step 2: GUI target 必验**

```bash
cd /Users/ls/MyWork/code/ylp_agent_learn/lp-agent/echo-agent-cli
cargo check --no-default-features --features gui --bin echo-agent-tauri
cargo test --no-default-features --features gui
```

Expected: 零错误

- [ ] **Step 3: clippy（推荐）**

```bash
cd /Users/ls/MyWork/code/ylp_agent_learn/lp-agent/echo-agent-cli
cargo clippy --all-targets -- -D warnings 2>&1 | tail -15
```

Expected: 零警告（若有关键警告，修复后再继续）

- [ ] **Step 4: 前端验证**

```bash
cd /Users/ls/MyWork/code/ylp_agent_learn/lp-agent/echo-agent-cli/web-frontend
npx tsc -b
npm run build
```

Expected: 零错误

- [ ] **Step 5: 回归确认**

确认以下未受影响：
- ObservabilityPanel 的 token/cache 展示（仍从 trace_collector 取数，未被改动）
- 现有累计 token 统计（TUI `app.tokens` 仍保留，只是 status bar 不再显示它）

- [ ] **Step 6: cargo clean 释放空间（AGENTS.md 强制）**

```bash
cd /Users/ls/MyWork/code/ylp_agent_learn/lp-agent/echo-agent-cli
cargo clean
```

> 本功能不动 echo-agent 框架，无需 clean echo-agent 的 target；但若期间编译过，也 clean 一下：
> `cd /Users/ls/MyWork/code/ylp_agent_learn/lp-agent/echo-agent && cargo clean`（仅当编译过时）

---

## 自审记录

**Spec 覆盖**：
- ✅ 数据源真实 prompt_tokens → Task 1（逻辑）+ Task 3（TUI 写入）+ Task 6（Web 写入）
- ✅ 进度条 + 百分比 + 颜色分级 → Task 1（逻辑）+ Task 4（TUI 渲染）+ Task 7（Web 渲染）
- ✅ 占位（首次响应前）→ Task 4 Step 1（`is_available` 判断）+ Task 7（`ctxUsage` null 判断）
- ✅ window_size 未知 → Task 1（`used_percentage` 返回 None）+ Task 4/7（不显示百分比）
- ✅ TUI/GUI 对等 → Task 4 与 Task 7 展示规格一致
- ✅ 修复 TUI 现有 bug → Task 2 Step 1（修正注释）+ Task 4（替换 request_count 显示）
- ✅ UTF-8 安全 → 进度条用 `repeat`/`format!`，无字节切片；数字格式化无 panic 路径
- ✅ 整数溢出防护 → Task 1 `used_percentage` 用 u64 中间值 + clamp

**类型一致性**：
- `ContextWindowSnapshot` 字段（Task 1）↔ TUI 写入（Task 3 Step 2）↔ TUI 读取（Task 4 Step 1）：`input_tokens`/`cached_tokens`/`cache_creation_tokens`/`output_tokens`/`context_window_size`/`updated_at` 一致 ✅
- `ContextWindowUsage`（Task 5/6）↔ Web 写入（Task 6）↔ Web 读取（Task 7）：`inputTokens`/`cachedTokens`/`cacheCreationTokens`/`outputTokens` 一致（已按 Task 6 修正删去 `contextWindowSize`）✅
- `format_token_count`（Rust，Task 1）↔ `formatTokens`（TS，Task 7 Step 3）：语义一致 ✅
