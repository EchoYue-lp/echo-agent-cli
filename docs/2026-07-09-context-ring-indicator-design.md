# 上下文圆环指示器（v2：圆环 + 缓存命中率）— 设计文档

- 日期：2026-07-09
- 状态：已批准（待实现）
- 范围：`echo-agent-cli`（应用层），零框架改动
- 前置：v1「上下文窗口占用指示器」已合并（commit `1d5fdf7`），本设计在其基础上升级视觉形态 + 新增缓存命中率

---

## 1. 背景与动机

v1 已实现上下文窗口占用指示器，但形态是「ASCII 进度条 + 文字」（如 `▓░░░░░░░░░ 15.2k/128k 12%`）。业界主流 agent（Claude Code / Cursor 等）用的是**圆环进度**，更紧凑、更直观。同时 v1 没有展示**缓存命中率**——这是衡量 token 成本的关键指标（高命中率 = 大幅省钱/提速）。

本次升级：
1. 把指示器从「ASCII 条」改成**圆环**（GUI 用 SVG 真圆环，TUI 用 unicode 圆字符近似）
2. 新增**会话平均缓存命中率**展示（GUI 在 hover tooltip，TUI 内联）

## 2. 现状调研（动手前先查「是不是已经有了」）

| 现状 | 位置 |
|---|---|
| v1 上下文指示器（ASCII 条 + 文字） | `status_bar.rs` / `ChatInput.tsx:665-687,970-995` |
| **缓存命中率算法已存在**（右边栏） | `Σcached/Σinput`（`diagnostics.rs:154`，前端 `ObservabilityPanel.tsx:48-64`） |
| 缓存数据全链路已通 | `LlmUsage` 事件携带 `cached_prompt_tokens`，前端 store 已存 `cachedTokens` |
| **缺口**：右边栏是「打开时重算」（遍历 trace 事件 + IPC），不适合常驻圆环 | 需要增量累加器 |
| **缺口**：`usage_reported` 没进 store | 需补传（区分「0% 命中」vs「无数据」） |
| ratatui 0.29 无圆环 widget | TUI 用 unicode 圆字符近似 |
| 前端无现成环形组件 | Web 用 SVG `stroke-dasharray` 自绘 |

## 3. 参考依据（AGENTS.md「列依据」）

| 参考 | 做法 | EKO 取舍 |
|---|---|---|
| **Claude Code**（TUI） | statusline 用 unicode 圆字符（`◴◒◐◓●`）画环 | **采纳**：TUI 用 unicode 圆字符，业界共识 |
| Cursor / 桌面 agent | SVG 圆环（`stroke-dasharray`） | **采纳**：GUI 用 SVG 真圆环 |
| 右边栏缓存命中率 | `Σcached/Σinput`（token 加权累计平均） | **复用公式**，范围改为「当前 conversation」 |

**关键判断**：TUI 受 cell-grid 限制无法画真圆，用 unicode 圆字符近似是**业界普遍做法**（Claude Code 亦然）。TUI/GUI「对等」指信息一致（都显示占用% + 缓存率），非像素一致——符合 AGENTS.md「多模式功能对等」的本意。

## 4. 框架 vs 应用层判定

**放应用层**。理由：缓存率算法虽在 `echo-agent-app-core::observability`，但「增量累加器」是 UI 投影需求（常驻展示），换产品不成立；且数据源 LlmUsage 事件框架已发，无需新增框架能力。

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
```

- **Snapshot** 答「现在占了多少」（每次覆盖，对应圆环填充比例）
- **Accumulator** 答「这个会话平均缓存命中多少」（每次累加，对应缓存率）
- 两者范围都是「当前 conversation」，重置点一致

### 5.2 数据模型（echo-agent-app-core 新增）

在 `context_window.rs` 新增：

```rust
/// 当前会话的 LLM 用量累计统计（用于缓存命中率等会话级指标）。
///
/// 与 ContextWindowSnapshot(瞬时占用) 的区别：本结构是累计式，
/// 每次 LlmUsage 累加；范围 = 当前 conversation，会话重置时清零。
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
    /// total_input=0 时返回 None（首条响应前）。
    pub fn cache_hit_rate(&self) -> Option<f64> {
        if self.total_input == 0 {
            return None;
        }
        Some(self.total_cached as f64 / self.total_input as f64)
    }
}
```

新增 unicode 圆字符进度辅助（TUI 用）：

```rust
/// 用 unicode 圆字符近似环形进度（5 档，离散近似）。
/// None → '○'（空环，首条响应前）。
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
- 颜色：`normal`→`var(--text-tertiary)` / `high`→`var(--color-warning)` / `critical`→`var(--color-error)` / 首次响应前→灰色空环中心 `--`
- **hover tooltip**（自定义浮层，非 title 属性——title 延迟且样式难控）：
  ```
  上下文容量：15.2k / 128k (12%)
  平均缓存命中率：98.7%
  ```
  - window 未知时：只显示绝对 token + 缓存率，不显示百分比行
  - 缓存率 None（首条响应前）：显示 `--`

#### TUI status bar（unicode 圆字符，内联）

```
EKO · coding · gpt-5 · thinking · ◓ 15.2k/128k 12% · cache 98%
```

- 圆字符 `○◔◑◓●`（5 档近似），用 `render_ring_char()`
- 紧跟占用数字 + %（与 v1 同）
- 末尾内联 `cache 98%`（TUI 无 hover，必须内联展示）
- 缓存率 None 时显示 `cache --`
- 首次响应前：`○ --/128k · cache --`

### 5.4 数据流

#### TUI 路径
1. `TuiApp` 新增字段 `usage_accumulator: ContextUsageAccumulator`
2. `events.rs` 的 LlmUsage 主循环分支（v1 已接住写 snapshot）：**追加**累加 `app.usage_accumulator.record(prompt_tokens, cached_prompt_tokens, usage_reported)`
   - 注意：需把 `usage_reported` 加进 TUI 本地 `AgentEvent::LlmUsage` 变体（v1 没带它）
3. `status_bar.rs`：圆字符用 `render_ring_char(pct)` 替换 ASCII 条；追加 `cache {rate}%` 显示（读 `app.usage_accumulator.cache_hit_rate()`）
4. 会话重置：TUI 是进程级会话（一次启动一个 conversation），无需显式重置

#### Web 路径
1. `ContextWindowUsage` 接口新增 `usageReported: boolean` 字段
2. `chatEventHandler.ts` 的 llm_usage 分支：补传 `usageReported: event.usage_reported`
3. `chatStore` 新增 `usageAccumulator: { totalInput, totalCached }` state + 在 `setContextWindow` 时**同时**累加（仅当 usageReported=true）
   - 或者：accumulator 作为独立 state，由 llm_usage 单独驱动累加
4. `ChatInput.tsx`：
   - 用 SVG 圆环替换现有 ASCII 条 span
   - 圆环读 `contextWindow`（占用%）+ 颜色 tier
   - tooltip 读 `contextWindow`（容量）+ `usageAccumulator.cacheHitRate`
5. 会话重置：`clearMessages` / `replaceMessages` 重置 accumulator（与 `contextWindow` 同步）

### 5.5 错误处理与边界

- **首条响应前**：snapshot `is_available()==false` + accumulator `total_input==0` → GUI 灰色空环中心 `--`、tooltip 缓存率 `--`；TUI `○ --/win · cache --`
- **window_size 未知（=0）**：GUI 圆环仍显示（只不显中心%），tooltip 只显绝对 token；TUI 不显%
- **usage_reported=false**：**不累加** accumulator（避免污染命中率）+ 后台 `tracing::warn!` 日志记录（便于及时发现 provider 未报 usage）。snapshot 也不更新（保持上次值，避免闪 0）
- **整数溢出**：accumulator 用 `u64` + `saturating_add`；缓存率用 `f64`
- **UTF-8 安全**：unicode 圆字符用 `char` 字面量，无字节切片

## 6. 不做什么（YAGNI）

- **不改 v1 的 snapshot 语义**：瞬时占用仍是覆盖式，缓存率走独立累加器——不混在一个结构里
- **不新增 IPC**：缓存率走 LlmUsage 事件增量累加，不调 `get_cache_diagnostics`（那是右边栏按需用的）
- **不改右边栏**：ObservabilityPanel 的诊断逻辑（按需重算）保持不变，本功能是独立的常驻累加器
- **TUI 不画真圆**：重构 status bar 单行布局成本太高，unicode 近似已足够（Claude Code 同款）
- **不落盘**：accumulator 是运行时投影，重启无需保留（EKO 本地助理定位）

## 7. 测试策略

- **单元测试**（echo-agent-app-core）：
  - `ContextUsageAccumulator::record`：累加正常值、`usage_reported=false` 不累加、saturating 防溢出
  - `cache_hit_rate`：0/50/100%、total_input=0 返回 None
  - `render_ring_char`：各百分比档位映射正确
- **前端**：SVG 圆环的 dashoffset 计算函数（纯函数，可测）
- **集成验证**：TUI 启动发消息看圆字符变化 + cache%；GUI hover 看 tooltip 完整内容

## 8. 验证清单（提交前）

- `cargo fmt --all` + `--check`（exit 0）
- `cargo check --workspace` + `cargo test --workspace`
- GUI target 必验：`--features gui --bin echo-agent-tauri` + `--features gui` test
- 前端：`npx tsc -b` + `npm run build`
- clippy `-D warnings`（改动代码零警告）
- 全部通过后 `cargo clean`
