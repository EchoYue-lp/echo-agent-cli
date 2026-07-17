# 上下文圆环指示器（v2：圆环 + 缓存命中率）— 设计文档

- 日期：2026-07-09
- 状态：已实现（待提交）
- 范围：`echo-agent-cli`（应用层），零框架改动
- 前置：v1「上下文窗口占用指示器」已合并（commit `1d5fdf7`），本设计在其基础上升级视觉形态 + 新增缓存命中率
- 修订：2026-07-09 审查补丁——补齐「压缩后 Snapshot 策略」与「重置点分表」（见 §3 / §5.4 / §5.5）

---

## 1. 背景与动机

v1 已实现上下文窗口占用指示器，但形态是「ASCII 进度条 + 文字」（如 `▓░░░░░░░░░ 15.2k/128k 12%`）。业界主流 agent（Claude Code / Cursor 等）用的是**圆环进度**，更紧凑、更直观。同时 v1 没有展示**缓存命中率**——这是衡量 token 成本的关键指标（高命中率 = 大幅省钱/提速）。

本次升级：
1. 把指示器从「ASCII 条」改成**圆环**（GUI 用 SVG 真圆环，TUI 用 unicode 圆字符近似）
2. 新增**会话平均缓存命中率**展示（GUI 在 hover tooltip，TUI 内联）
3. **补齐压缩 / `/clear` 边界上的占用重置策略**（v1 缺口：压缩后圆环长期显示虚高占用）

## 2. 现状调研（动手前先查「是不是已经有了」）

| 现状 | 位置 |
|---|---|
| v1 上下文指示器（ASCII 条 + 文字） | `status_bar.rs` / `ChatInput.tsx:665-687,970-995` |
| Snapshot 语义（覆盖式 `prompt_tokens`） | `context_window.rs`（已对齐 Claude Code） |
| **缓存命中率算法已存在**（右边栏） | `Σcached/Σinput`（`diagnostics.rs:154`，前端 `ObservabilityPanel.tsx:48-64`） |
| 缓存数据全链路已通 | `LlmUsage` 事件携带 `cached_prompt_tokens`，前端 store 已存 `cachedTokens` |
| 框架已发 `ContextCompressed` | `compact.rs` → `AgentEvent::ContextCompressed{before/after_tokens}`（本地 tokenizer 估算） |
| **缺口**：右边栏是「打开时重算」（遍历 trace 事件 + IPC），不适合常驻圆环 | 需要增量累加器 |
| **缺口**：`usage_reported` 没进 TUI 本地事件 / Web store | 需补传（区分「0% 命中」vs「无数据」） |
| **缺口**：`ContextCompressed` 不更新 Snapshot；Web 未映射该事件 | 压缩后圆环虚高，直到下一轮 `LlmUsage` |
| **缺口**：TUI `/clear` 重置 agent，但不清 `context_snapshot` | 空会话仍显示旧 % |
| ratatui 0.29 无圆环 widget | TUI 用 unicode 圆字符近似 |
| 前端无现成环形组件 | Web 用 SVG `stroke-dasharray` 自绘 |

## 3. 参考依据（AGENTS.md「列依据」）

| 参考 | 做法 | EKO 取舍 |
|---|---|---|
| **Claude Code**（TUI） | statusline 用 unicode 圆字符画环；占用 = 最近一次 API 的 input tokens（覆盖式，非会话累计） | **采纳**：TUI unicode 圆 + Snapshot 覆盖式语义 |
| **Claude Code**（压缩边界） | 官方文档：`/compact` 后 `current_usage` → `null`，等下一次 API 再填；历史上有「压缩后 statusline 仍显示旧 %」的 bug（#41541 / #19669），社区期望立刻刷新 | **采纳方案 A**：压缩后 Snapshot 置空（圆环 `--` / `○`），下一轮 `LlmUsage` 再填——诚实、复用「首条响应前」占位语义 |
| **Cursor** | SVG 圆环；自动 summarize 后占用 % 明显下降（用户可见 jump down） | **采纳** GUI SVG 圆环；下降体感由「置空 → 下一轮真实值」达成（不把本地 `after_tokens` 伪装成权威） |
| **Codex** | 压缩后上下文变成 summary + 近期消息；下一轮 usage 反映新窗口；累计账单与「当前窗口」分开 | **采纳**：Snapshot（瞬时占用）与 Accumulator（会话累计缓存率）分表；压缩只动前者 |
| 右边栏缓存命中率 | `Σcached/Σinput`（token 加权累计平均） | **复用公式**，范围改为「当前 conversation」 |

**关键判断**：

1. TUI 受 cell-grid 限制无法画真圆，用 unicode 圆字符近似是**业界普遍做法**（Claude Code 亦然）。TUI/GUI「对等」指信息一致（都显示占用% + 缓存率），非像素一致——符合 AGENTS.md「多模式功能对等」的本意。
2. **压缩 ≠ 会话重置**：圆环/占用% 应在压缩后下降或先置未知；缓存命中率（会话累计）跨压缩保留。不要用「会话累计 input」当圆环填充——Claude Code 曾踩过这个坑（#13783）。

## 4. 框架 vs 应用层判定

**放应用层**。理由：

- 缓存率累加器是 UI 投影需求（常驻展示），换产品不成立。
- 数据源 `LlmUsage` / `ContextCompressed` 框架已发，无需新增框架能力。
- Web 缺 `ContextCompressed` 映射、TUI `/clear` 不清 Snapshot——都是应用层接线缺口，在 CLI/前端补齐即可（仍属「零框架改动」）。

## 5. 设计

### 5.1 架构总览

新增「会话累计累加器」`ContextUsageAccumulator`，与现有 `ContextWindowSnapshot`（瞬时占用）并存，各自职责清晰：

```
LlmUsage 事件(已有,框架发射)
  ├─→ ContextWindowSnapshot    (v1,瞬时占用,覆盖式) → 圆环本体 + 占用%
  └─→ ContextUsageAccumulator  (新增,累计式)        → 缓存命中率
        每次: total_input += prompt_tokens
              total_cached += cached_prompt_tokens   (仅当 usage_reported=true)
        cache_hit_rate() = total_cached / total_input

ContextCompressed 事件(已有,框架发射)
  └─→ ContextWindowSnapshot    → 置空（unavailable）  ※ 不动 Accumulator
```

- **Snapshot** 答「现在占了多少」（每次覆盖，对应圆环填充比例）
- **Accumulator** 答「这个会话平均缓存命中多少」（每次累加，对应缓存率）
- **conversation 边界**（`/clear`、新会话、`clearMessages`、`replaceMessages`）：两者都清零
- **压缩边界**（auto-compact / `/compact` / `compress_context`）：**只动 Snapshot，不动 Accumulator**

### 5.2 数据模型（echo-agent-app-core 新增）

在 `context_window.rs` 新增：

```rust
/// 当前会话的 LLM 用量累计统计（用于缓存命中率等会话级指标）。
///
/// 与 ContextWindowSnapshot(瞬时占用) 的区别：本结构是累计式，
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
```

在 `ContextWindowSnapshot` 上补充（或旁路 helper）：

```rust
impl ContextWindowSnapshot {
    /// 压缩边界 / 会话边界：置为 unavailable。
    /// 圆环回到「首条响应前」占位（`--` / `○`），等下一轮 LlmUsage 再填。
    /// 保留 context_window_size（模型上限不变）。
    pub fn clear_usage(&mut self) {
        self.input_tokens = 0;
        self.cached_tokens = 0;
        self.cache_creation_tokens = 0;
        self.output_tokens = 0;
        self.updated_at = None;
        // context_window_size 保留
    }
}
```

新增 unicode 圆字符进度辅助（TUI 用）：

```rust
/// 用 unicode 圆字符近似环形进度（5 档，离散近似）。
/// None → '○'（空环：首条响应前，或刚压缩后）。
/// 这是 TUI 受 cell-grid 限制的近似做法（Claude Code 同款）。
pub fn render_ring_char(used_percentage: Option<u16>) -> char {
    match used_percentage {
        None | Some(0) => '○',
        Some(1..=25) => '◔',    // ~1/4 填充
        Some(26..=50) => '◑',   // ~1/2 填充
        Some(51..=75) => '◓',   // ~3/4 填充
        Some(_) => '●',         // 接近满 / 满
    }
}
```

> 注：unicode 圆字符的视觉填充是离散近似的（5 档），与 GUI 的连续 SVG 环形态不同——这是 TUI 天然限制，业界（含 Claude Code）普遍接受。

### 5.3 展示规格

#### GUI 圆环（ChatInput footer，SVG 自绘）

```
       ╭───╮
      ╱     ╲     ← 外环按 used% 填充,颜色随 tier(绿/黄/红)
     │  12%  │    ← 中心显示占用% 数字
      ╲     ╱
       ╰───╯
```

- SVG 实现：`<circle>` 背景 + `<circle>` 前景用 `stroke-dasharray` + `stroke-dashoffset` 按 ratio 填充
- 尺寸：约 28×28px（紧凑，适配 footer）
- 中心文字：占用%（如 `12%`），颜色随 tier
- 颜色：`normal`→`var(--text-tertiary)` / `high`→`var(--color-warning)` / `critical`→`var(--color-error)` / 不可用（首条响应前或刚压缩后）→灰色空环中心 `--`
- **hover tooltip**（自定义浮层，非 title 属性——title 延迟且样式难控）：
  ```
  上下文容量：15.2k / 128k (12%)
  平均缓存命中率：98.7%
  ```
  - window 未知时：只显示绝对 token + 缓存率，不显示百分比行
  - Snapshot unavailable：容量行显示 `--`（或「压缩后待刷新」——YAGNI，先用 `--`）
  - 缓存率 None：显示 `--`

#### TUI status bar（unicode 圆字符，内联）

```
EKO · coding · gpt-5 · thinking · ◓ 15.2k/128k 12% · cache 98%
```

- 圆字符 `○◔◑◓●`（5 档近似），用 `render_ring_char()`
- 紧跟占用数字 + %（与 v1 同）
- 末尾内联 `cache 98%`（TUI 无 hover，必须内联展示）
- 缓存率 None 时显示 `cache --`
- Snapshot unavailable（首条响应前 / 刚压缩后）：`○ --/128k · cache {rate或--}`
  - 注意：压缩后 cache 率**仍可显示**（Accumulator 未清），只有占用数字变 `--`

### 5.4 数据流与重置点

#### 事件 → 状态（权威表）

| 事件 | Snapshot | Accumulator | 说明 |
|---|---|---|---|
| `LlmUsage`（`usage_reported=true`） | 覆盖为 `prompt_tokens` 等 | `record(...)` | 权威占用来自 provider |
| `LlmUsage`（`usage_reported=false`） | **不更新**（保持上次） | **不累加** | 避免闪 0 / 污染命中率；后台 `tracing::warn!` |
| `ContextCompressed`（auto / 手动） | **`clear_usage()`** | **不动** | 方案 A；下一轮 LlmUsage 再填 |
| `/clear`、新会话、`clearMessages` | `clear_usage()` | `reset()` | conversation 边界，双清 |
| `replaceMessages`（加载历史） | `clear_usage()` | `reset()` | 等下次 LLM；不从历史回放（YAGNI） |

> **禁止**：用 `ContextCompressed.after_tokens` 直接写 Snapshot 权威占用。该字段是本地 tokenizer 估算，与 provider `prompt_tokens` 可能偏离。若将来要做 interim 估算（方案 B），必须在 UI 上标注「估算」，本 v2 不做。

#### TUI 路径

1. `TuiApp` 新增字段 `usage_accumulator: ContextUsageAccumulator`
2. `events.rs` 的 `LlmUsage` 主循环分支（v1 已接住写 snapshot）：
   - 把 `usage_reported` 加进 TUI 本地 `AgentEvent::LlmUsage` 变体（v1 没带它）
   - `usage_reported=true` 时覆盖 snapshot + `usage_accumulator.record(...)`
   - `usage_reported=false` 时跳过两者 + `tracing::warn!`
3. `events.rs` 的 `ContextCompressed` 分支（v1 只打系统消息）：**追加** `app.context_snapshot.clear_usage()`（**不**动 accumulator）
4. `/clear` 处理（`events.rs`）：在 `agent.reset()` 之后，追加 `context_snapshot.clear_usage()` + `usage_accumulator.reset()`（与 Web `clearMessages` 对等）
5. `status_bar.rs`：圆字符用 `render_ring_char(pct)` 替换 ASCII 条；追加 `cache {rate}%`（读 `app.usage_accumulator.cache_hit_rate()`）

#### Web 路径

1. `ContextWindowUsage` 接口新增 `usageReported: boolean` 字段
2. `chatEventHandler.ts` 的 `llm_usage` 分支：
   - 补传 `usageReported: event.usage_reported`
   - `usage_reported=false` 时不调用 `setContextWindow` / 不累加
3. **新增** `ChatEvent::ContextCompressed` 映射（`chat.rs` 目前未映射——实现前置）：
   - handler 调用 `clearContextWindow()`（或等价：`contextWindow = null`），**不**重置 accumulator
4. `chatStore` 新增 `usageAccumulator: { totalInput, totalCached }` state
   - 由 `llm_usage` 单独驱动累加（仅当 `usageReported=true`）
   - 提供 `clearContextWindow()` / `resetUsageAccumulator()`；`clearMessages` / `replaceMessages` 两者都调
5. 手动压缩 IPC（`compress_context`）：成功后须发出与 auto-compact 相同的 `ContextCompressed` 事件（或前端在 IPC 成功回调里直接 `clearContextWindow()`）。**两条路径最终效果必须一致**：Snapshot 置空、Accumulator 保留。
6. `ChatInput.tsx`：
   - 用 SVG 圆环替换现有 ASCII 条 span
   - 圆环读 `contextWindow`（占用%）+ 颜色 tier；unavailable 时灰色空环 + `--`
   - tooltip 读 `contextWindow`（容量）+ `usageAccumulator.cacheHitRate`

### 5.5 错误处理与边界

- **首条响应前 / 刚压缩后**：snapshot `is_available()==false` → GUI 灰色空环中心 `--`；TUI `○ --/win`。Accumulator 可能仍有值（压缩后）或为 0（首条前）——分别显示 `cache N%` / `cache --`
- **window_size 未知（=0）**：GUI 圆环仍显示（只不显中心%），tooltip 只显绝对 token；TUI 不显%
- **usage_reported=false**：**不累加** accumulator + **不更新** snapshot（保持上次值，避免闪 0）+ 后台 `tracing::warn!`
- **手动 `/compact` 后无后续 LLM 调用**：圆环保持 `--`（unavailable），直到用户再发消息——这是方案 A 的预期行为，不是 bug
- **整数溢出**：accumulator 用 `u64` + `saturating_add`；缓存率用 `f64`
- **UTF-8 安全**：unicode 圆字符用 `char` 字面量，无字节切片

## 6. 不做什么（YAGNI）

- **不改 v1 的 snapshot 语义**：瞬时占用仍是覆盖式，缓存率走独立累加器——不混在一个结构里
- **不用 `ContextCompressed.after_tokens` 写权威占用**（方案 B / interim 估算留给将来，须带「估算」标注）
- **不新增 IPC**：缓存率走 LlmUsage 事件增量累加；完整单 run 诊断由 durable `RunStore` 的观测面板按需查询。
- **不改右边栏**：ObservabilityPanel 的诊断逻辑（按需重算）保持不变，本功能是独立的常驻累加器
- **TUI 不画真圆**：重构 status bar 单行布局成本太高，unicode 近似已足够（Claude Code 同款）
- **不落盘**：accumulator 是运行时投影，重启无需保留（EKO 本地助理定位）
- **不从历史消息回放重建 accumulator**：`replaceMessages` 直接清零，等下次 LLM

## 7. 测试策略

- **单元测试**（echo-agent-app-core）：
  - `ContextUsageAccumulator::record`：累加正常值、`usage_reported=false` 不累加、saturating 防溢出
  - `cache_hit_rate`：0/50/100%、total_input=0 返回 None
  - `reset`：清零后 `cache_hit_rate()==None`
  - `ContextWindowSnapshot::clear_usage`：`is_available()==false`，`context_window_size` 保留
  - `render_ring_char`：各百分比档位映射正确；`None` → `○`
- **边界行为测试**（应用层 / 集成级，按现有测试风格落地）：
  - 压缩后：Snapshot unavailable，Accumulator 数值不变
  - 下一轮 `LlmUsage`：Snapshot 覆盖为新的较低 `prompt_tokens`
  - `/clear`（TUI）与 `clearMessages`（Web）：Snapshot + Accumulator 双清
  - `usage_reported=false`：两者都不动
- **前端**：SVG 圆环的 dashoffset 计算函数（纯函数，可测）
- **集成验证**：TUI 启动发消息看圆字符变化 + cache%；手动 `/compact` 后圆环变 `--`、cache% 仍在；再发消息后占用回填；GUI hover 看 tooltip 完整内容

## 8. 验证清单（提交前）

- `cargo fmt --all` + `--check`（exit 0）
- `cargo check --workspace` + `cargo test --workspace`
- GUI target 必验：`--features gui --bin echo-agent-tauri` + `--features gui` test
- 前端：`npx tsc -b` + `npm run build`
- clippy `-D warnings`（改动代码零警告）
- 全部通过后 `cargo clean`

## 9. 实现清单（按依赖序）

1. `context_window.rs`：`ContextUsageAccumulator` + `clear_usage` + `render_ring_char` + 单测
2. TUI：`usage_reported` 进本地事件 → LlmUsage 门控写 snapshot/accumulator → `ContextCompressed` 调 `clear_usage` → `/clear` 双清 → status_bar 圆环 + cache%
3. Web：`ChatEvent::ContextCompressed` 映射 → handler 清 Snapshot → store accumulator + `usageReported` 门控 → `clearMessages`/`replaceMessages` 双清 → `compress_context` 成功路径与 auto-compact 对齐 → ChatInput SVG 圆环 + tooltip
4. 跑 §8 验证清单
