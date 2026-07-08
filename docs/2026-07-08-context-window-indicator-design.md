# 上下文窗口占用指示器 — 设计文档

- 日期：2026-07-08
- 状态：已批准（待实现）
- 范围：`echo-agent-cli`（应用层），零框架改动
- 关联：TUI 与 GUI 功能对等（AGENTS.md 硬约束）

---

## 1. 背景与动机

EKO 当前缺少"当前会话上下文长度使用情况"的展示——这是 Claude Code / Codex / Cursor 等所有主流 agent 都有的功能。良好的上下文长度可视化能：

- **提升任务准确率**：用户能感知上下文是否即将耗尽，及时 `/compact`，避免上下文污染导致的幻觉与指令遵从下降
- **减少 token 浪费**：上下文接近满载时，每次请求都在重传巨量历史；及时压缩能显著降本
- **对标标杆**：Claude Code statusline 把 `context_window.used_percentage` 作为核心字段始终展示

## 2. 现状调研（动手前先查"是不是已经有了"）

| 环节 | 现状 | 关键位置 |
|---|---|---|
| Provider usage 字段 | ✅ 完整（prompt/completion/total + cache 细分） | `echo-core/src/llm/types.rs:655-684` |
| OpenAI/Anthropic 开 `include_usage` | ✅ 主动开启 | `openai.rs:194`, `anthropic.rs:338-360` |
| 框架事件 `AgentEvent::LlmUsage` | ✅ 已发射，携带 prompt_tokens + cache + total | `echo-core/src/agent/mod.rs:40-67`，发射点 `think.rs:183-206` |
| **"当前上下文长度"语义** | ❌ **核心缺口**——prompt_tokens 只被累加成历史总量 | `TokenUsageTracker`（`echo-core/src/tokenizer.rs:222`）只 `fetch_add` |
| TUI status bar 的 "tokens" | 🐛 现有 bug：显示 `request_count / 1000`，非真实 token；`LlmUsage` 被显式丢弃 | `status_bar.rs:32`, `events.rs:849-867` |
| Web 聊天主界面 | ❌ `llm_usage` 事件 no-op，聊天界面无任何 token 展示 | `chatEventHandler.ts:60-64` |
| 上下文窗口上限 | ✅ `agent.config().get_token_limit()`（默认 128K，可配） | `infra.rs:20`，`panels.rs:1015` |
| 估算器 | ⚠️ `HeuristicTokenizer`（启发式 + EMA 校准），不如真实 prompt_tokens 准 | `echo-core/src/tokenizer.rs:48` |

**结论**：数据全链路已通到事件层，缺的是"接住事件 → 维持当前快照 → 渲染"这最后一公里。本设计补齐它，**不动框架**。

## 3. 参考依据（AGENTS.md 要求"关键决策列依据"）

| 参考实现 | 做法 | EKO 取舍 |
|---|---|---|
| **Claude Code** statusline | `used_percentage = (input + cache_creation + cache_read) / context_window_size`；始终可见；颜色分级绿/黄/红；社区 issue #15247 共识："别只在 95% 才弹，要始终可见" | **采纳**：真实 prompt_tokens 数据源、始终可见、颜色分级 |
| Codex `--json` | usage 走事件流，无内置 statusline | 参考：usage 走事件流（EKO 已有 `LlmUsage`，对齐） |
| Cursor / Devin | plan artifact / approval gate，与 token 展示无关 | 不相关 |

**为何用真实 prompt_tokens 而非估算**：精度最高（provider 实测）、框架已发事件、顺带修复 TUI 现有 bug；与 Claude Code 共识一致。估算仅在"首次响应前无数据"时作占位兜底（显示 `--`，不强行显示 0）。

## 4. 框架 vs 应用层判定

**放应用层**（AGENTS.md"拿不准默认放应用层"）。理由：
- 数据源（`LlmUsage` 事件、`token_limit`）框架已具备，无需新增框架能力
- "最近一次响应快照"是 UI 投影概念，换产品不成立
- 应用层下沉容易，框架污染难清理

## 5. 设计

### 5.1 架构总览

```
echo-agent 框架(不动)
  └─ AgentEvent::LlmUsage { prompt_tokens, cached_prompt_tokens,
                            cache_creation_prompt_tokens, total_tokens, ... }
     (已有, think.rs:183 发射)

echo-agent-cli 应用层(本功能全部在此)
  ├─ echo-agent-app-core: 新增 ContextWindowSnapshot + 写入逻辑
  │    (TUI 路径与 GUI 路径各自接 LlmUsage 后写入同一快照结构)
  ├─ TUI: events.rs 接 LlmUsage(不再丢弃) → 写 snapshot → status_bar.rs 渲染真实百分比
  └─ Web: chat.rs 替换 no-op → 发 Tauri 事件 → chatEventHandler 接住 → Zustand store → ChatInput footer 渲染
```

### 5.2 数据模型（echo-agent-app-core 新增）

```rust
/// 当前会话上下文窗口占用快照(来自最近一次 LLM 响应)。
///
/// 语义:这是"当前上下文长度",不是累计消耗 —— 每次 LLM 调用后覆盖。
/// 对齐 Claude Code 的 context_window.used_percentage 语义。
#[derive(Clone, Default, Debug)]
pub struct ContextWindowSnapshot {
    /// 本次请求的实际输入 token(= 当前上下文主体),已含 cache 部分。
    pub input_tokens: u32,
    /// 其中命中缓存的部分(参考用,展示时可单独标注 cached)。
    pub cached_tokens: u32,
    /// 写入缓存的部分(参考用)。
    pub cache_creation_tokens: u32,
    /// 本次生成 token(不计入"占用",仅参考)。
    pub output_tokens: u32,
    /// 模型上下文窗口上限(来自 agent token_limit;0 表示未知)。
    pub context_window_size: u32,
    /// 首次响应前为 None → UI 显示占位。
    pub updated_at: Option<Instant>,
}

impl ContextWindowSnapshot {
    /// 占用百分比 = input_tokens / context_window_size。
    /// window_size 为 0(未知)时返回 None,UI 不显示百分比。
    pub fn used_percentage(&self) -> Option<u16> {
        if self.context_window_size == 0 { return None; }
        // u32 运算,用 u64 中间值防溢出
        let pct = (self.input_tokens as u64) * 100 / (self.context_window_size as u64);
        Some(pct.clamp(0, 100) as u16)
    }
}
```

**百分比公式**（对齐 Claude Code）：
```
used_percentage = input_tokens / context_window_size × 100
```
（`input_tokens` 本身就是 provider 返回的总输入 token，已含 cache_creation + cache_read，不重复加。这与 Claude Code 文档的 `input + cache_creation + cache_read` 等价。）

### 5.3 展示规格（TUI / GUI 信息完全对等）

| 信息元素 | 内容 |
|---|---|
| 进度条 | 10 格 ASCII：`▓` 已用 / `░` 剩余 |
| 绝对数字 | `15.2k / 128k`（k 单位，便于扫读；<1000 时显示原数） |
| 百分比 | `12%` |
| 颜色分级 | 🟢 `<70%` / 🟡 `70-89%` / 🔴 `≥90%` |
| 占位（首次响应前） | `-- / 128k`（灰色，不显示 0% 与进度条） |
| window_size 未知 | 仅显示绝对 token `15.2k`，不显示百分比与进度条 |

**TUI 示例**（status_bar.rs，复用现有 `app.tokens` 字段位但修正语义为真实 prompt_tokens）：
```
EKO · coding · gpt-5 · thinking · ▓░░░░░░░░░ 15.2k/128k 12%
```

**Web ChatInput footer 示例**（在现有 `模型 · 权限 · 模式` 状态行后追加一段）：
```
gpt-5 · auto · ... · [▓░░░░░░░░░ 15.2k/128k · 12%]
```

### 5.4 数据流细节

**TUI 路径**：
1. `events.rs:849` 当前 `LlmUsage` 分支被丢弃 → 改为：从事件取 `prompt_tokens` + cache 字段，写入 `app.context_window_snapshot`（同时保留现有 trace 日志）
2. `context_window_size` 来源：`agent.config().get_token_limit()`（agent 切换模型时已同步，见 `agent_pool.rs:419`）
3. `status_bar.rs:32` 现有 `tokens.2 / 1000`（request_count）→ 改为读 snapshot 的 `used_percentage` + `input_tokens`
4. 现有 `app.tokens`（累计 request_count）**保留**用于其它统计，不删

**Web 路径**：
1. `tauri/commands/chat.rs:1009` 的 `LlmUsage` 分支当前写 trace + usage_store → **追加**：构造 `ContextWindowSnapshot` 并通过 Tauri emit 一个新事件（如 `chat://context-window`）
2. `chatEventHandler.ts:60` 当前 no-op → 改为：接住新事件，写入 Zustand store 的 `contextWindow` 字段
3. `ChatInput.tsx` footer 读取 store，渲染进度条 + 百分比
4. `context_window_size`：**随 snapshot 一起下发**（前端无需单独初始化拉取，避免时序问题）。后端在构造 snapshot 时从 `agent.config().get_token_limit()` 取值填入 `context_window_size` 字段

### 5.5 错误处理与边界

- **首次响应前**：`snapshot.updated_at == None` → 显示占位 `--`，不显示 0%（避免误导）
- **`context_window_size` 未知（=0）**：只显示绝对 token `15.2k`，不显示百分比与进度条
- **`prompt_tokens` 缺失**（个别 provider 不报 usage）：保持上次快照不更新，UI 不闪 0
- **UTF-8 安全**：进度条用 `String::push_str` 逐字符拼接，数字格式化用 `format!`，零字节切片（符合 AGENTS.md Rust 硬约束）
- **整数溢出**：百分比计算用 `u64` 中间值 + `clamp`，符合"禁止溢出 panic"
- **模型切换**：agent_pool 切换模型时已 `set_token_limit`，下次 LlmUsage 写入时同步刷新 window_size

## 6. 不做什么（YAGNI）

- **不联动压缩**：压缩是否触发仍由 `ContextManager` 按配置阈值决定，本功能只展示。高阈值提醒（"建议 /compact"）留作后续增强，不在此版本
- **不落盘**：快照是运行时投影，重启无需保留（对齐 EKO 本地助理定位，无多用户/审计需求）
- **不改框架**：`echo-agent` 零改动（符合"放应用层"判定）
- **不加 OpenTelemetry 指标**：框架现有 `record_llm_tokens` 是死代码，但清理它超出本功能范围，不在此次触碰

## 7. 测试策略

- **单元测试**（echo-agent-app-core）：
  - `ContextWindowSnapshot::used_percentage()` 的边界：0%、50%、100%、>100%（clamp）、window_size=0（None）
  - 数字格式化：`format_token_count(0) / (999) / (1500) / (128000)` → `0` / `999` / `1.5k` / `128k`
  - 进度条生成：0% / 5% / 50% / 95% / 100% 的字符数正确
- **集成验证**：
  - TUI：启动后发一条消息，确认 status bar 显示真实 token（而非 request_count/1000）
  - Web：启动 GUI 后发消息，确认 footer 出现进度条且随对话增长
- **回归**：确认现有累计 token 统计（ObservabilityPanel 等）未受影响

## 8. 验证清单（提交前）

按 AGENTS.md "提交前必须验证"：
- `cargo check --workspace`（根 crate）
- 逐 crate test（`verify-all-crates.sh --quick`，因本功能不动 echo-agent 框架，框架侧逐 crate 仅作回归）
- **GUI target 必验**：`cargo check --no-default-features --features gui --bin echo-agent-tauri` + `cargo test --no-default-features --features gui`
- `cargo fmt --all` + `cargo fmt --all -- --check`（退出码 0）
- 前端：`cd web-frontend && npx tsc -b && npm run build`
- 全部通过后 `cargo clean` 释放空间，再提交
