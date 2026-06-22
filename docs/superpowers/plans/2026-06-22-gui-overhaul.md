# EKO GUI 全面优化实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 修复 EchoCoWork GUI 的 5 类显示问题（布局割裂 / markdown 未渲染 / Token 卡重复 / Token 全 0 假数据）并顺带替换 `window.prompt` 计划编辑器，让任务运行时面板达到生产可用质量。

**Architecture:** 分 6 阶段推进——A 阶段修后端 usage 透传根因（框架层）；B 阶段补 markdown 渲染（含 GFM 表格）；C 阶段拆解"任务执行大盒子"为消息流独立卡片；D 阶段 Token 卡去重；E 阶段计划编辑器；F 阶段打磨。每阶段独立可测、可单独提交。前端无测试框架，改用 `tsc -b` 类型检查 + 手动验证清单；Rust 端写标准 `#[cfg(test)]` 单测。

**Tech Stack:** Tauri 2 / React 19 / TypeScript / Tailwind 4 / Zustand（前端）；Rust / tokio / serde（后端 `echo-agent` + `echo-agent-app-core`）。

**已确认的关键事实（写代码时直接用）：**
- `AgentEvent::LlmUsage` 字段：`model: String, prompt_tokens: u64, completion_tokens: u64, total_tokens: u64, cached_prompt_tokens: u64, cache_creation_prompt_tokens: u64, usage_reported: bool`
- `SubagentResult`（`echo-agent/src/agent/subagent/types.rs:138`）当前只有 `tokens_used: Option<usize>`，无完整 usage
- `SubagentExecutor::dispatch_fork`（`echo-agent/src/agent/subagent/executor.rs:545`）`AgentEvent::LlmUsage { .. } => {}` 空匹配，**丢弃真实 usage**
- `delegate_to_agent_with_parent_and_cancel`（`echo-agent/src/agent/react/mod.rs:1955`）返回 `Result<String>`，`Ok(result.output)` 丢弃 `SubagentResult`
- `executor.rs:982-985` 只读路径写假 `unavailable_llm_usage_payload("readonly_delegate_path_does_not_expose_provider_usage_yet")`
- `record_worker_llm_usage`（`store.rs:1079`）接受 `serde_json::Value` payload，扩展字段无需改签名
- `MarkdownContent`（`web-frontend/src/components/common/MarkdownContent.tsx`）已被 `MessageBubble` 使用，靠 `renderMarkdown`（`utils/markdown.ts`）+ DOMPurify；**不支持 GFM 表格**，CSS 也无 `.md-content table` 样式
- `TaskRuntimePanel.tsx` 用 `ScrollableText`（纯文本）渲染 worker 结果 / 最终结果；`ConversationTimeline.tsx` 全文无 `MarkdownContent`
- `AppLayout.tsx` 三栏：左 sidebar 300px + 中 `ChatPanel` + 右 `RightRail`
- `ChatPanel.tsx` 消息流：`messages.map → <MessageBubble/>` + `<ConversationTimeline/>` + `<TaskRuntimeMainPanel/>`（整个大盒子）
- `delegate_to_agent_with_cancel` 在 `mod.rs:1944` 转发到 `delegate_to_agent_with_parent_and_cancel`，无其他外部调用方

---

## 文件结构（改动总览）

**后端（Rust）—— 阶段 A：**
- 修改 `echo-agent/src/agent/subagent/types.rs`：`SubagentResult` 增加 `usage` 字段
- 新建 `echo-agent/src/agent/subagent/usage.rs`：`LlmUsageStats` 结构 + 累加器
- 修改 `echo-agent/src/agent/subagent/executor.rs`：`dispatch_fork` 捕获 `LlmUsage`，填充 `SubagentResult.usage`
- 修改 `echo-agent/src/agent/react/mod.rs`：`delegate_to_agent_with_parent_and_cancel` 返回 `SubagentResult`；`delegate_to_agent_with_cancel` 调整
- 修改 `echo-agent-app-core/src/tasks/task_runtime/executor.rs`：只读路径用真实 usage，删假占位
- 新建 `echo-agent/src/agent/subagent/usage_tests.rs`（或在 types.rs 内 `#[cfg(test)]`）：单测

**前端（React）—— 阶段 B/C/D/E/F：**
- 修改 `web-frontend/src/utils/markdown.ts`：补 GFM 表格解析
- 修改 `web-frontend/src/index.css`：补 `.md-content table` 样式
- 修改 `web-frontend/src/components/common/MarkdownContent.tsx`：加 `maxHeight` prop + 可滚动容器
- 修改 `web-frontend/src/components/common/ScrollableText.tsx`：内部改用 `MarkdownContent`（或废弃，调用点直接换）
- 修改 `web-frontend/src/components/task/TaskRuntimePanel.tsx`：worker 结果/最终结果接 MarkdownContent；拆解大盒子；Token 卡去重
- 修改 `web-frontend/src/components/chat/ConversationTimeline.tsx`：接 MarkdownContent
- 修改 `web-frontend/src/components/chat/ChatPanel.tsx`：调整挂载方式支持独立卡片
- 新建 `web-frontend/src/components/task/PlanEditor.tsx`：计划编辑器
- 新建 `web-frontend/src/components/task/RuntimeStoryCard.tsx`：拆出的独立卡片组件
- 新建 `web-frontend/src/components/task/ResultFullView.tsx`：长结果全屏抽屉

---

## 阶段 A：后端真实 usage 透传（问题 5 根因）

### Task A1: 扩展 SubagentResult 携带完整 usage 结构

**Files:**
- Create: `echo-agent/src/agent/subagent/usage.rs`
- Modify: `echo-agent/src/agent/subagent/types.rs:138`
- Modify: `echo-agent/src/agent/subagent/mod.rs`（加 `pub mod usage;`）

- [ ] **Step 1: 新建 usage.rs 定义 LlmUsageStats 与累加器**

创建 `echo-agent/src/agent/subagent/usage.rs`：

```rust
use serde::{Deserialize, Serialize};

/// 单次 LLM 调用的 usage 快照，字段对齐 `AgentEvent::LlmUsage`。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct LlmUsageStats {
    pub model: String,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
    pub cached_prompt_tokens: u64,
    pub cache_creation_prompt_tokens: u64,
    /// 任一调用上报过即视为 true；全部未上报才 false。
    pub usage_reported: bool,
    /// 累计的 LLM 调用次数。
    pub call_count: u64,
}

impl LlmUsageStats {
    /// 累加一次 LlmUsage 事件。model 取最近一次（同一次 dispatch 内通常一致）。
    pub fn record(
        &mut self,
        model: &str,
        prompt_tokens: u64,
        completion_tokens: u64,
        total_tokens: u64,
        cached_prompt_tokens: u64,
        cache_creation_prompt_tokens: u64,
        usage_reported: bool,
    ) {
        self.model = model.to_string();
        self.prompt_tokens += prompt_tokens;
        self.completion_tokens += completion_tokens;
        self.total_tokens += total_tokens;
        self.cached_prompt_tokens += cached_prompt_tokens;
        self.cache_creation_prompt_tokens += cache_creation_prompt_tokens;
        if usage_reported {
            self.usage_reported = true;
        }
        self.call_count += 1;
    }

    /// 转成前端 `cacheUsageFromEvents` 期望的 payload 形态。
    pub fn to_payload(&self, session_id: &str) -> serde_json::Value {
        serde_json::json!({
            "session_id": session_id,
            "model": if self.model.is_empty() { "unknown" } else { &self.model },
            "prompt_tokens": self.prompt_tokens,
            "completion_tokens": self.completion_tokens,
            "total_tokens": self.total_tokens,
            "cached_prompt_tokens": self.cached_prompt_tokens,
            "cache_creation_prompt_tokens": self.cache_creation_prompt_tokens,
            "usage_reported": self.usage_reported,
            "call_count": self.call_count,
        })
    }
}
```

- [ ] **Step 2: 在 mod.rs 暴露模块**

在 `echo-agent/src/agent/subagent/mod.rs` 顶部模块声明区加入：

```rust
pub mod usage;
```

- [ ] **Step 3: SubagentResult 增加 usage 字段**

修改 `echo-agent/src/agent/subagent/types.rs` 中 `SubagentResult` 结构体（约 138 行），在 `tokens_used` 字段后增加：

```rust
use crate::agent::subagent::usage::LlmUsageStats;

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct SubagentResult {
    // ...既有字段保持不变...
    pub output: String,
    pub tokens_used: Option<usize>,
    /// 本次 dispatch 累计的 LLM usage；若 agent 未上报任何 usage 则为 None。
    #[serde(default)]
    pub usage: Option<LlmUsageStats>,
}
```

> 注意：`tokens_used` 保留以兼容既有调用方，新逻辑改用 `usage`。

- [ ] **Step 4: 编译验证**

Run: `cd echo-agent && cargo build -p echo-agent`
Expected: 编译通过（dispatch_fork 还没填 usage，`usage` 默认 None，不破坏现有行为）

- [ ] **Step 5: 写 LlmUsageStats 单测**

在 `echo-agent/src/agent/subagent/usage.rs` 末尾加：

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accumulates_multiple_calls() {
        let mut stats = LlmUsageStats::default();
        stats.record("claude-x", 100, 50, 150, 80, 10, true);
        stats.record("claude-x", 200, 60, 260, 150, 20, true);
        assert_eq!(stats.prompt_tokens, 300);
        assert_eq!(stats.completion_tokens, 110);
        assert_eq!(stats.total_tokens, 410);
        assert_eq!(stats.cached_prompt_tokens, 230);
        assert_eq!(stats.call_count, 2);
        assert!(stats.usage_reported);
    }

    #[test]
    fn usage_reported_stays_false_until_any_true() {
        let mut stats = LlmUsageStats::default();
        stats.record("m", 10, 5, 15, 0, 0, false);
        assert!(!stats.usage_reported);
        stats.record("m", 10, 5, 15, 0, 0, true);
        assert!(stats.usage_reported);
    }

    #[test]
    fn payload_uses_unknown_when_model_empty() {
        let stats = LlmUsageStats::default();
        let p = stats.to_payload("sess-1");
        assert_eq!(p["model"], serde_json::json!("unknown"));
        assert_eq!(p["usage_reported"], serde_json::json!(false));
    }
}
```

- [ ] **Step 6: 运行测试**

Run: `cd echo-agent && cargo test -p echo-agent subagent::usage`
Expected: 3 tests passed

- [ ] **Step 7: Commit**

```bash
cd /Users/ls/MyWork/code/ylp_agent_learn/lp-agent
git add echo-agent/src/agent/subagent/usage.rs echo-agent/src/agent/subagent/mod.rs echo-agent/src/agent/subagent/types.rs
git commit -m "feat(subagent): add LlmUsageStats and usage field to SubagentResult"
```

---

### Task A2: dispatch_fork 捕获 LlmUsage 填充 SubagentResult

**Files:**
- Modify: `echo-agent/src/agent/subagent/executor.rs:485-560`（dispatch_fork 主循环）

- [ ] **Step 1: 在 dispatch_fork 内引入 LlmUsageStats 累加器**

定位 `dispatch_fork` 函数（约 `executor.rs:485`），在创建 `SubagentResult` 初始值后、事件循环前，加入累加器。找到构造 `let mut result = SubagentResult { ... }` 处，在其后加：

```rust
let mut usage_stats = crate::agent::subagent::usage::LlmUsageStats::default();
```

- [ ] **Step 2: 替换空匹配 LlmUsage 分支**

将 `executor.rs:545` 的：
```rust
AgentEvent::LlmUsage { .. } => {}
```
替换为：
```rust
AgentEvent::LlmUsage {
                    model,
                    prompt_tokens,
                    completion_tokens,
                    total_tokens,
                    cached_prompt_tokens,
                    cache_creation_prompt_tokens,
                    usage_reported,
                } => {
                    usage_stats.record(
                        model,
                        prompt_tokens,
                        completion_tokens,
                        total_tokens,
                        cached_prompt_tokens,
                        cache_creation_prompt_tokens,
                        usage_reported,
                    );
                }
```

- [ ] **Step 3: 循环结束后回填 result.usage**

在事件循环结束、`Ok(result)` 返回前（或在已有 `result.tokens_used = ...` 赋值附近）加：

```rust
if usage_stats.call_count > 0 {
    result.usage = Some(usage_stats);
    // 同步保持 tokens_used 兼容（prompt+completion 合计）
    result.tokens_used = Some(
        (usage_stats.prompt_tokens + usage_stats.completion_tokens) as usize,
    );
}
```

- [ ] **Step 4: 编译验证**

Run: `cd echo-agent && cargo build -p echo-agent`
Expected: 编译通过

- [ ] **Step 5: 写集成测试验证 dispatch 透传 usage**

在 `executor.rs` 末尾 `#[cfg(test)]` 模块（如无则新建）加：

```rust
#[cfg(test)]
mod usage_propagation_tests {
    use super::*;
    // 注：以下测试依赖能构造最小 SubagentExecutor 的 test fixture。
    // 若现有代码已有 test fixture 工厂（如 `fn test_executor()`），直接复用；
    // 否则用 mock agent channel 注入一条 LlmUsage 事件。
    //
    // 若 SubagentExecutor 构造依赖过重无法单测，则标记 ignore 并在 Step 6 用
    // 手动端到端验证代替，本步骤保留测试骨架供 fixture 就绪后启用。

    #[tokio::test]
    #[ignore = "启用条件：SubagentExecutor 测试 fixture 就绪"]
    async fn dispatch_fork_propagates_llm_usage() {
        // TODO(fixture): 用 test fixture 构造 executor，注入一条 LlmUsage 事件，
        // assert result.usage.unwrap().prompt_tokens == 期望值
    }
}
```

> 说明：`SubagentExecutor` 构造依赖 registry/event_bus/agent factory，若现有代码无轻量 fixture，此测试标记 `#[ignore]`，靠 Task A4 Step 5 端到端验证。`#[ignore]` 测试保留为占位骨架——当后续建立 executor fixture 时填入断言：构造 executor → 注入一条 `AgentEvent::LlmUsage{prompt_tokens:100, ...}` → 调 dispatch → 断言 `result.usage.unwrap().prompt_tokens == 100 && call_count == 1`。不要在 fixture 就绪前删除此测试。

- [ ] **Step 6: 运行非忽略测试确认无回归**

Run: `cd echo-agent && cargo test -p echo-agent subagent -- --skip ignored`
Expected: 既有测试全 pass，新测试被 skip

- [ ] **Step 7: Commit**

```bash
git add echo-agent/src/agent/subagent/executor.rs
git commit -m "feat(subagent): capture LlmUsage events in dispatch_fork"
```

---

### Task A3: delegate API 返回 SubagentResult

**Files:**
- Modify: `echo-agent/src/agent/react/mod.rs:1944-2015`（两个 delegate 方法）

- [ ] **Step 1: delegate_to_agent_with_parent_and_cancel 返回 SubagentResult**

定位 `mod.rs:1955` 的 `delegate_to_agent_with_parent_and_cancel`，将其返回类型从 `Result<String, String>` 改为 `Result<crate::agent::subagent::SubagentResult, String>`，并把末尾 `Ok(result.output)` 改为 `Ok(result)`。

签名变为：
```rust
pub async fn delegate_to_agent_with_parent_and_cancel(
    &self,
    parent: &str,
    target: &str,
    task: &str,
    cancel: tokio_util::sync::CancellationToken,
) -> Result<crate::agent::subagent::SubagentResult, String>
```
末尾：
```rust
Ok(result)  // 原 Ok(result.output)
```

- [ ] **Step 2: delegate_to_agent_with_cancel 调整转发**

`mod.rs:1944` 的 `delegate_to_agent_with_cancel` 同样改返回类型为 `Result<SubagentResult, String>`，内部已转发到 parent 版本，无需改逻辑（透传 `SubagentResult`）。

- [ ] **Step 3: 检查所有调用方适配**

Run: `cd echo-agent && cargo build -p echo-agent 2>&1 | grep -E "error\[|--> " | head -30`
Expected: 列出所有因返回类型变化导致的编译错误位置（主要是取 `.output` 的调用点）

对每个错误点：把 `delegate_xxx(...).await` 的结果从 `String` 改为 `SubagentResult`，调用方需要 `.output` 字段处加 `.output`。典型模式：
```rust
// 改前
let out = self.delegate_to_agent_with_cancel(...).await?;
// 改后
let res = self.delegate_to_agent_with_cancel(...).await?;
let out = res.output;
// 若需要 usage，额外用 res.usage
```

逐一修复直至 `cargo build -p echo-agent` 通过。

- [ ] **Step 4: 编译整个 workspace 确认无遗漏**

Run: `cd /Users/ls/MyWork/code/ylp_agent_learn/lp-agent && cargo build`
Expected: 全 workspace 编译通过

- [ ] **Step 5: 运行全部 Rust 测试确认无回归**

Run: `cd /Users/ls/MyWork/code/ylp_agent_learn/lp-agent && cargo test`
Expected: 既有测试全 pass

- [ ] **Step 6: Commit**

```bash
git add echo-agent/src/agent/react/mod.rs
# 以及 Step 3 修复的其他文件
git commit -m "refactor(react): delegate API returns SubagentResult to expose usage"
```

---

### Task A4: executor.rs 只读路径用真实 usage，删假占位

**Files:**
- Modify: `echo-agent-app-core/src/tasks/task_runtime/executor.rs:960-1000`（run_readonly_worker 调用处）
- Modify: `echo-agent-app-core/src/tasks/task_runtime/executor.rs:1202+`（run_readonly_worker 函数体）
- Modify: `echo-agent-app-core/src/tasks/task_runtime/executor.rs:1443+`（unavailable_llm_usage_payload 保留但仅用于真无数据场景）

- [ ] **Step 1: run_readonly_worker 返回 SubagentResult 而非 String**

定位 `executor.rs:1202` 附近 `fn run_readonly_worker`，当前返回 `Result<String, String>`。改为接收 `SubagentResult` 并返回它。函数签名改为：

```rust
async fn run_readonly_worker(
    ...既有参数...
) -> Result<crate::agent::subagent::SubagentResult, String> {
    let result = self
        .runtime
        .main_agent()
        .delegate_to_agent_with_parent_and_cancel(...).await?;
    Ok(result)  // 原 Ok(result.output)，现在透传 SubagentResult
}
```

> 内部若对 `result.output` 有后续处理（如提取摘要），保留那段逻辑，但函数最终返回完整 `SubagentResult`。

- [ ] **Step 2: 调用点用真实 usage 替换假占位**

定位 `executor.rs:960-1000` 调用 `run_readonly_worker` 处。当前代码（982-985）写假 `unavailable_llm_usage_payload`。替换为：

```rust
let worker_result = self.run_readonly_worker(...).await;  // 已有

match worker_result {
    Ok(sub_result) => {
        // 先记录 worker 文本输出（既有逻辑）
        // ...既有 store.record_worker_output 等调用，用 sub_result.output...

        // 用真实 usage 替换假占位
        let usage_payload = if let Some(ref stats) = sub_result.usage {
            stats.to_payload(run_id)
        } else {
            // 真无数据时才用 unavailable 占位（保留语义，但这是真无上报，非路径缺陷）
            unavailable_llm_usage_payload("provider_returned_no_usage_for_readonly_worker")
        };
        store.record_worker_llm_usage(
            run_id, task_id, &worker_id, &agent_name, &title, usage_payload,
        )?;

        Ok(sub_result.output)  // 上游仍需 String 输出
    }
    Err(e) => { ...既有错误处理... }
}
```

> 关键：删除原 `unavailable_llm_usage_payload("readonly_delegate_path_does_not_expose_provider_usage_yet")` 这条假数据。

- [ ] **Step 3: 编译验证**

Run: `cd /Users/ls/MyWork/code/ylp_agent_learn/lp-agent && cargo build`
Expected: 编译通过

- [ ] **Step 4: 写 executor 单测验证不再写假 usage**

在 `executor.rs` 的 `#[cfg(test)]` 模块加（若 executor 测试 fixture 过重则标记 ignore 并用 Task A4 Step 5 手动验证）：

```rust
#[tokio::test]
#[ignore = "启用条件：TaskRuntime executor 测试 fixture 就绪"]
async fn readonly_worker_records_real_usage_not_placeholder() {
    // 当 fixture 就绪时填入：注入 mock agent 返回带 usage 的 SubagentResult，
    // 断言 store 中 worker_llm_usage 事件的 usage_reported == true 且 prompt_tokens > 0，
    // 且不含 reason == "readonly_delegate_path_does_not_expose_provider_usage_yet"。
}
```

- [ ] **Step 5: 手动端到端验证**

启动应用，发起一个只读并行委派任务（如"审查当前项目"），观察：
- 后端日志/数据库 `tr_usage_records` 表有非 0 的 `input_tokens`/`output_tokens`
- 前端右侧栏 Token/Cache 卡显示真实数值（非全 0、model 非 unknown）
- 不再出现 "provider usage 缺失" 诊断

若 provider 确实不返回 usage（某些本地模型），则卡片显示 `usage_reported=false` 的真实状态，而非假 0。

- [ ] **Step 6: Commit**

```bash
git add echo-agent-app-core/src/tasks/task_runtime/executor.rs
git commit -m "fix(executor): record real LLM usage for readonly delegate workers"
```

---

### Task A5: 前端 cacheUsageFromEvents 过滤假数据（防御层）

**Files:**
- Modify: `web-frontend/src/components/task/TaskRuntimePanel.tsx:301-420`（cacheUsageFromEvents + summary 计算）

- [ ] **Step 1: cacheUsageFromEvents 跳过 usage_reported=false 事件**

定位 `TaskRuntimePanel.tsx:301` 的 `cacheUsageFromEvents`。在累加循环里，对每个 `worker_llm_usage` 事件先判断 `usage.usage_reported`，false 的不计入主统计，单独计一个 `unreportedCount`。

修改累加逻辑（伪代码示意，实际改对应行）：
```typescript
let unreportedCount = 0;
for (const ev of events) {
  if (ev.kind === 'worker_llm_usage') {
    const u = ev.payload.usage;
    if (!u.usage_reported) {
      unreportedCount++;
      continue;  // 不污染主统计
    }
    // ...既有累加 prompt_tokens 等...
  }
}
return { ...既有字段..., unreportedCount };
```

- [ ] **Step 2: CacheUsageCard 区分"未上报"与"0 token"**

定位 `CacheUsageCard` 组件（同文件内），在显示 `calls` 时附加：若 `unreportedCount > 0`，显示 "N calls (M 未上报)" 而非笼统 "8 calls"。

- [ ] **Step 3: 类型检查**

Run: `cd web-frontend && npx tsc -b --pretty`
Expected: 无类型错误

- [ ] **Step 4: 手动验证**

与 Task A4 Step 5 同场景：确认前端显示与后端真实数据一致。

- [ ] **Step 5: Commit**

```bash
git add web-frontend/src/components/task/TaskRuntimePanel.tsx
git commit -m "fix(ui): filter unreported usage events in cacheUsageFromEvents"
```

---

## 阶段 B：markdown 渲染补齐（问题 2/3 根因）

### Task B1: renderMarkdown 支持 GFM 表格

**Files:**
- Modify: `web-frontend/src/utils/markdown.ts`（在现有解析流程中加表格分支）
- Modify: `web-frontend/src/utils/markdown.ts` sanitize 配置（ALLOWED_TAGS 加表格标签）

- [ ] **Step 1: 阅读现有 renderMarkdown 结构**

Run: `cd web-frontend && head -120 src/utils/markdown.ts`
确认现有解析是逐行状态机还是正则替换。本计划假设是逐行处理（基于已有代码风格）。

- [ ] **Step 2: 添加表格解析函数**

在 `markdown.ts` 内加一个独立函数：

```typescript
/**
 * 解析 GFM 表格块。输入是连续的若干行（首行表头、次行分隔、余下数据行）。
 * 返回 HTML 字符串。非表格输入返回 null。
 */
function renderGfmTable(lines: string[]): string | null {
  if (lines.length < 2) return null;
  const splitRow = (line: string) =>
    line.trim().replace(/^\|/, '').replace(/\|$/, '').split('|').map(c => c.trim());
  const header = splitRow(lines[0]);
  const separator = lines[1].trim();
  // 分隔行必须形如 |---|:--:|---:|
  if (!/^\|?[\s:-]*-{3,}[\s:-|]*\|?$/.test(separator) && !separator.includes('-')) return null;
  if (!separator.split('|').every(cell => /^[\s:-]*-{2,}[\s:-]*$/.test(cell.trim()))) return null;
  const rows = lines.slice(2).map(splitRow);
  const thead = `<thead><tr>${header.map(h => `<th>${escapeHtml(h)}</th>`).join('')}</tr></thead>`;
  const tbody = `<tbody>${rows
    .map(r => `<tr>${r.map(c => `<td>${escapeHtml(c)}</td>`).join('')}</tr>`)
    .join('')}</tbody>`;
  return `<div class="md-table-wrap"><table>${thead}${tbody}</table></div>`;
}

function escapeHtml(s: string): string {
  return s
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;');
}
```

> 若 `markdown.ts` 已有 `escapeHtml`/等价函数，复用之，勿重复定义。

- [ ] **Step 3: 在主解析流程接入表格分支**

在逐行处理循环里，当遇到一行含 `|` 且下一行匹配分隔行模式时，向后贪心收集连续表格行，调用 `renderGfmTable`，输出 HTML 跳过这些行。

- [ ] **Step 4: sanitize 白名单加表格标签**

定位 `markdown.ts` 中 `DOMPurify.sanitize` 的配置（约 170 行），在 `ALLOWED_TAGS` 数组加入：`'table','thead','tbody','tr','th','td'`，`ALLOWED_ATTR` 加 `'class'`（用于 `md-table-wrap`）。

- [ ] **Step 5: 类型检查 + 手动验证**

Run: `cd web-frontend && npx tsc -b --pretty`
手动：在 MessageBubble 输入一段含表格的 markdown，确认渲染成 HTML 表格。

- [ ] **Step 6: Commit**

```bash
git add web-frontend/src/utils/markdown.ts
git commit -m "feat(markdown): support GFM tables in renderMarkdown"
```

---

### Task B2: 补充表格 CSS 样式

**Files:**
- Modify: `web-frontend/src/index.css`（在 `.md-content` 样式块附近）

- [ ] **Step 1: 添加表格样式**

在 `index.css` 的 `.md-content` 规则块后追加：

```css
.md-content .md-table-wrap,
.md-pre-wrap .md-table-wrap {
  overflow-x: auto;
  max-width: 100%;
  margin: 0.5rem 0;
}

.md-content table,
.md-pre-wrap table {
  border-collapse: collapse;
  width: 100%;
  font-size: 0.875rem;
}

.md-content th,
.md-content td,
.md-pre-wrap th,
.md-pre-wrap td {
  border: 1px solid var(--border, #d1d5db);
  padding: 0.375rem 0.625rem;
  text-align: left;
  vertical-align: top;
}

.md-content th,
.md-pre-wrap th {
  background: var(--bg-muted, #f3f4f6);
  font-weight: 600;
}

.md-content tr:nth-child(even),
.md-pre-wrap tr:nth-child(even) {
  background: var(--bg-subtle, #fafafa);
}
```

> CSS 变量名以项目既有 `:root` 变量为准；若不存在，用注释里的 fallback 值。

- [ ] **Step 2: 手动验证渲染效果**

启动应用，发送含表格 markdown，确认表格有边框、表头背景、斑马纹、横向滚动（窄屏时）。

- [ ] **Step 3: Commit**

```bash
git add web-frontend/src/index.css
git commit -m "style(markdown): add table styles for GFM tables"
```

---

### Task B3: MarkdownContent 支持 maxHeight 可滚动

**Files:**
- Modify: `web-frontend/src/components/common/MarkdownContent.tsx`

- [ ] **Step 1: 加 maxHeight prop 与滚动容器**

读取当前 `MarkdownContent.tsx`，找到根 `<div>`。改为：

```tsx
interface MarkdownContentProps {
  content: string;
  /** 设置后内容区限高并可纵向滚动；不设则自然展开。 */
  maxHeight?: number | string;
  className?: string;
}

export function MarkdownContent({ content, maxHeight, className }: MarkdownContentProps) {
  const html = useMemo(() => renderMarkdown(content), [content]);
  const style: React.CSSProperties = maxHeight
    ? { maxHeight, overflowY: 'auto' }
    : {};
  return (
    <div
      className={cn('md-content', className)}
      style={style}
      dangerouslySetInnerHTML={{ __html: html }}
    />
  );
}
```

> `cn` 用项目既有类名合并工具（若无则 `className` 直接拼接）。保留既有复制按钮等逻辑（若复制按钮挂在外层，相应调整）。

- [ ] **Step 2: 类型检查**

Run: `cd web-frontend && npx tsc -b --pretty`
Expected: 无错误（既有调用方不传 maxHeight，行为不变）

- [ ] **Step 3: Commit**

```bash
git add web-frontend/src/components/common/MarkdownContent.tsx
git commit -m "feat(MarkdownContent): support maxHeight prop for scrollable content"
```

---

### Task B4: worker 结果区接 MarkdownContent

**Files:**
- Modify: `web-frontend/src/components/task/TaskRuntimePanel.tsx`（WorkerTraceRow 内 result 渲染处，约 797-805）

- [ ] **Step 1: 替换 ScrollableText 为 MarkdownContent**

定位 `WorkerTraceRow` 内渲染 `result`/`output` 的 `<ScrollableText text={...}/>`，替换为：

```tsx
<MarkdownContent content={workerResult} maxHeight={520} />
```

> `workerResult` 取原传给 ScrollableText 的同源文本。`maxHeight={520}` 保持原视觉高度上限，但内容渲染为 markdown。

- [ ] **Step 2: 检查 ScrollableText 是否还有其他用途**

Run: `cd web-frontend && grep -rn "ScrollableText" src/`
若仅剩最终结果区（Task B5 会处理）和无用引用，可在 B5 后整体删除 `ScrollableText.tsx`；若仍有合法用途则保留。

- [ ] **Step 3: 类型检查 + 手动验证**

Run: `npx tsc -b --pretty`
手动：发起并行委派任务，展开 worker 卡片，确认结果区渲染 markdown（标题/列表/表格/代码块）且可滚动。

- [ ] **Step 4: Commit**

```bash
git add web-frontend/src/components/task/TaskRuntimePanel.tsx
git commit -m "feat(ui): render worker results as markdown"
```

---

### Task B5: 最终任务结果区接 MarkdownContent

**Files:**
- Modify: `web-frontend/src/components/task/TaskRuntimePanel.tsx`（finalResult 渲染处，约 1512）

- [ ] **Step 1: 替换最终结果 ScrollableText**

定位 `TaskRuntimeMainPanel` 内渲染 `finalResult` 的 `<ScrollableText text={finalResult}/>`，替换为：

```tsx
<MarkdownContent content={finalResult} maxHeight={640} />
```

- [ ] **Step 2: 若 ScrollableText 已无引用则删除**

Run: `cd web-frontend && grep -rn "ScrollableText" src/`
若无结果，删除 `web-frontend/src/components/common/ScrollableText.tsx`。若有残留引用，逐一评估改为 `MarkdownContent` 或保留。

- [ ] **Step 3: 类型检查 + 手动验证**

Run: `npx tsc -b --pretty`
手动：确认"最终任务结果"区 markdown 渲染正常、可滚动、表格不溢出。

- [ ] **Step 4: Commit**

```bash
git add web-frontend/src/components/task/TaskRuntimePanel.tsx web-frontend/src/components/common/ScrollableText.tsx
git commit -m "feat(ui): render final task result as markdown, remove ScrollableText"
```

---

### Task B6: ConversationTimeline 接 MarkdownContent

**Files:**
- Modify: `web-frontend/src/components/chat/ConversationTimeline.tsx`（所有 `{event.content}` / `{w.summary}` 纯文本渲染处）

- [ ] **Step 1: 全量替换纯文本为 MarkdownContent**

Run: `cd web-frontend && grep -n "{event.content}\|{w.summary}\|{event.text}" src/components/chat/ConversationTimeline.tsx`
对每个匹配点，把 `{x}` 替换为 `<MarkdownContent content={x} />`（无限高，自然展开）。

- [ ] **Step 2: 类型检查 + 手动验证**

Run: `npx tsc -b --pretty`
手动：观察对话时间线里的事件文案渲染为 markdown。

- [ ] **Step 3: Commit**

```bash
git add web-frontend/src/components/chat/ConversationTimeline.tsx
git commit -m "feat(ui): render conversation timeline content as markdown"
```

---

## 阶段 C：拆解"任务执行大盒子"为消息流独立卡片（问题 1 根因）

### Task C1: 新建 RuntimeStoryCard 独立卡片组件

**Files:**
- Create: `web-frontend/src/components/task/RuntimeStoryCard.tsx`

- [ ] **Step 1: 抽取卡片外壳组件**

```tsx
import { ReactNode } from 'react';
import { cn } from '../../utils/cn'; // 用项目既有 cn

interface RuntimeStoryCardProps {
  /** 卡片标题，如"路由决策""并行执行" */
  title: string;
  /** 左侧时间线圆点状态 */
  status?: 'pending' | 'active' | 'done' | 'error';
  /** 是否默认折叠（内容多时） */
  defaultCollapsed?: boolean;
  children: ReactNode;
}

export function RuntimeStoryCard({
  title,
  status = 'done',
  defaultCollapsed = false,
  children,
}: RuntimeStoryCardProps) {
  const [collapsed, setCollapsed] = useState(defaultCollapsed);
  return (
    <section className="runtime-story-card my-3 rounded-lg border border-border bg-card">
      <header
        className="flex items-center gap-2 px-4 py-2 cursor-pointer select-none"
        onClick={() => setCollapsed(c => !c)}
      >
        <span className={cn('story-dot', `story-dot--${status}`)} />
        <h3 className="text-sm font-medium flex-1">{title}</h3>
        <span className="text-xs text-muted">{collapsed ? '展开' : '收起'}</span>
      </header>
      {!collapsed && <div className="px-4 pb-3">{children}</div>}
    </section>
  );
}
```

> `useState` 从 react 导入（顶部加 import）。类名 `border-border`/`bg-card`/`text-muted` 以项目 Tailwind 主题变量为准；若项目用 `border-zinc-200` 等具体色值，相应替换。

- [ ] **Step 2: 补卡片样式（可选，若 Tailwind 类不够）**

在 `index.css` 加：
```css
.story-dot { width: 8px; height: 8px; border-radius: 50%; display: inline-block; }
.story-dot--done { background: #10b981; }
.story-dot--active { background: #3b82f6; }
.story-dot--pending { background: #9ca3af; }
.story-dot--error { background: #ef4444; }
```

- [ ] **Step 3: 类型检查**

Run: `cd web-frontend && npx tsc -b --pretty`
Expected: 无错误（组件尚未被使用）

- [ ] **Step 4: Commit**

```bash
git add web-frontend/src/components/task/RuntimeStoryCard.tsx web-frontend/src/index.css
git commit -m "feat(ui): add RuntimeStoryCard component for timeline cards"
```

---

### Task C2: TaskRuntimeMainPanel 改用独立卡片，去除大盒子边框

**Files:**
- Modify: `web-frontend/src/components/task/TaskRuntimePanel.tsx`（TaskRuntimeMainPanel 函数，约 1109-1530）

- [ ] **Step 1: 移除外层 section 大盒子**

定位 `TaskRuntimeMainPanel` 最外层 `<section className="...task-runtime...">`，移除其边框/背景/标题"任务执行"包装。改为返回一个 `<>` Fragment，内部每个 `RuntimeStoryStep` 用 `<RuntimeStoryCard>` 包裹。

- [ ] **Step 2: 每个 step 包成独立卡片**

对每个 `RuntimeStoryStep`（路由决策/计划确认/并行执行/最终结果/产出/文件变更/审查/测试验证），改为：
```tsx
<RuntimeStoryCard title="路由决策" status="done">
  {/* 原 step 内容 */}
</RuntimeStoryCard>
```

- [ ] **Step 3: "最终任务结果"卡片默认展开，其余可折叠**

最终结果卡片 `defaultCollapsed={false}`；过程类（路由决策/文件变更等）`defaultCollapsed={true}`，减少初始视觉负担。

- [ ] **Step 4: 类型检查 + 手动验证**

Run: `npx tsc -b --pretty`
手动：发起任务，确认主区每个阶段是独立圆角卡片，不再有"任务执行"大边框包裹全部。过程卡片默认折叠，最终结果展开。

- [ ] **Step 5: Commit**

```bash
git add web-frontend/src/components/task/TaskRuntimePanel.tsx
git commit -m "refactor(ui): break task runtime into independent story cards"
```

---

### Task C3: ChatPanel 挂载方式让卡片与消息同级

**Files:**
- Modify: `web-frontend/src/components/chat/ChatPanel.tsx`（消息流渲染处，约 103-151）

- [ ] **Step 1: 确认 ChatPanel 已在消息流末尾渲染 TaskRuntimeMainPanel**

读取 `ChatPanel.tsx:103-151`，确认结构为 `messages.map(MessageBubble)` + `ConversationTimeline` + `TaskRuntimeMainPanel`。经 C2 改造后，`TaskRuntimeMainPanel` 返回 Fragment 内多个 `RuntimeStoryCard`，它们会自然作为消息流同级元素流入。

- [ ] **Step 2: 确保滚动容器包含这些卡片**

确认外层滚动 `div`（含 `overflow-y-auto`）包裹了 `TaskRuntimeMainPanel`，卡片随消息流一起滚动。若 `TaskRuntimeMainPanel` 被放在滚动容器外，移入容器内。

- [ ] **Step 3: 手动验证**

启动应用，发任务，确认卡片在消息流中自然排布，与用户输入/agent 回复视觉层级一致，可随主区滚动。

- [ ] **Step 4: Commit**

```bash
git add web-frontend/src/components/chat/ChatPanel.tsx
git commit -m "refactor(ui): task runtime cards flow as first-class messages"
```

---

## 阶段 D：Token/Cache 卡片去重（问题 4 根因）

### Task D1: 删除 worker 卡片内 compact CacheUsageCard

**Files:**
- Modify: `web-frontend/src/components/task/TaskRuntimePanel.tsx`（WorkerTraceRow 内，约 795）

- [ ] **Step 1: 移除 worker 卡内 Token 卡**

定位 `WorkerTraceRow` 内 `<CacheUsageCard compact ... />`，整段删除。单 worker 的 token 数据对用户决策无价值，移到侧栏统一看。

- [ ] **Step 2: 类型检查 + 手动验证**

Run: `npx tsc -b --pretty`
手动：展开 worker 卡片，确认不再有 Token/Cache 小卡。

- [ ] **Step 3: Commit**

```bash
git add web-frontend/src/components/task/TaskRuntimePanel.tsx
git commit -m "refactor(ui): remove per-worker token card from worker trace"
```

---

### Task D2: 主区移除完整 CacheUsageCard，侧栏保留唯一一份

**Files:**
- Modify: `web-frontend/src/components/task/TaskRuntimePanel.tsx`（TaskRuntimeMainPanel 末尾，约 1517-1525）
- Verify: `web-frontend/src/components/layout/RightRail.tsx` 已有侧栏版本（约 170+）

- [ ] **Step 1: 移除主区末尾 Token 卡**

定位 `TaskRuntimeMainPanel` 末尾的 `<CacheUsageCard ... />`（完整版），整段删除。Token/Cache 是过程指标，归侧栏。

- [ ] **Step 2: 确认侧栏 RightRail 已有 CacheUsageCard**

Run: `cd web-frontend && grep -n "CacheUsageCard" src/components/layout/RightRail.tsx src/components/task/TaskRuntimePanel.tsx`
确认仅 `RightRail.tsx`（侧栏精简版）保留。若侧栏版本也用了 compact，视情况升级为完整版以承载迁移来的数据。

- [ ] **Step 3: 类型检查 + 手动验证**

Run: `npx tsc -b --pretty`
手动：确认主区无 Token 卡，侧栏有且仅有一份，显示真实数据（依赖阶段 A）。

- [ ] **Step 4: Commit**

```bash
git add web-frontend/src/components/task/TaskRuntimePanel.tsx
git commit -m "refactor(ui): move token/cache card to sidebar only"
```

---

## 阶段 E：计划编辑器替换 window.prompt

### Task E1: 新建 PlanEditor 组件

**Files:**
- Create: `web-frontend/src/components/task/PlanEditor.tsx`

- [ ] **Step 1: 编写 PlanEditor 组件**

```tsx
import { useState, useEffect } from 'react';

interface PlanTask {
  id: string;
  title: string;
  description?: string;
  status?: string;
}

interface PlanEditorProps {
  /** 初始任务列表 */
  initialTasks: PlanTask[];
  /** 保存回调，返回新的 JSON 字符串 */
  onSave: (tasksJson: string) => Promise<void> | void;
  onClose: () => void;
}

export function PlanEditor({ initialTasks, onSave, onClose }: PlanEditorProps) {
  const [tasks, setTasks] = useState<PlanTask[]>(initialTasks);
  const [rawJson, setRawJson] = useState(() => JSON.stringify(initialTasks, null, 2));
  const [error, setError] = useState<string | null>(null);
  const [mode, setMode] = useState<'form' | 'json'>('form');

  // 同步 form 编辑到 rawJson
  useEffect(() => {
    setRawJson(JSON.stringify(tasks, null, 2));
  }, [tasks]);

  const applyJson = () => {
    try {
      const parsed = JSON.parse(rawJson);
      if (!Array.isArray(parsed)) throw new Error('计划必须是任务数组');
      setTasks(parsed);
      setError(null);
    } catch (e) {
      setError(e instanceof Error ? e.message : 'JSON 解析失败');
    }
  };

  const updateTask = (id: string, patch: Partial<PlanTask>) => {
    setTasks(ts => ts.map(t => (t.id === id ? { ...t, ...patch } : t)));
  };

  const handleSave = async () => {
    await onSave(JSON.stringify(tasks));
    onClose();
  };

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/40">
      <div className="w-[720px] max-h-[80vh] rounded-lg bg-background border border-border flex flex-col">
        <header className="flex items-center justify-between px-4 py-3 border-b">
          <h2 className="font-medium">编辑任务计划</h2>
          <div className="flex gap-2">
            <button
              className={mode === 'form' ? 'font-semibold' : 'text-muted'}
              onClick={() => setMode('form')}
            >表单</button>
            <button
              className={mode === 'json' ? 'font-semibold' : 'text-muted'}
              onClick={() => setMode('json')}
            >JSON</button>
            <button onClick={onClose} className="text-muted">✕</button>
          </div>
        </header>
        <div className="flex-1 overflow-y-auto p-4">
          {mode === 'form' ? (
            <div className="space-y-3">
              {tasks.map(t => (
                <div key={t.id} className="space-y-1 border rounded p-2">
                  <input
                    className="w-full bg-transparent text-sm font-medium"
                    value={t.title}
                    onChange={e => updateTask(t.id, { title: e.target.value })}
                  />
                  <textarea
                    className="w-full bg-transparent text-xs"
                    rows={2}
                    value={t.description ?? ''}
                    onChange={e => updateTask(t.id, { description: e.target.value })}
                  />
                </div>
              ))}
            </div>
          ) : (
            <div className="space-y-2">
              <textarea
                className="w-full font-mono text-xs p-2 border rounded min-h-[300px]"
                value={rawJson}
                onChange={e => setRawJson(e.target.value)}
              />
              <button onClick={applyJson} className="text-xs px-2 py-1 border rounded">应用 JSON</button>
              {error && <div className="text-xs text-red-500">{error}</div>}
            </div>
          )}
        </div>
        <footer className="flex justify-end gap-2 px-4 py-3 border-t">
          <button onClick={onClose} className="px-3 py-1 text-sm border rounded">取消</button>
          <button onClick={handleSave} className="px-3 py-1 text-sm bg-primary text-primary-foreground rounded">保存</button>
        </footer>
      </div>
    </div>
  );
}
```

> Tailwind 主题类（`bg-background`/`bg-primary` 等）以项目既有配色变量为准；若用具体色值则相应替换。

- [ ] **Step 2: 类型检查**

Run: `cd web-frontend && npx tsc -b --pretty`
Expected: 无错误

- [ ] **Step 3: Commit**

```bash
git add web-frontend/src/components/task/PlanEditor.tsx
git commit -m "feat(ui): add PlanEditor modal component"
```

---

### Task E2: 替换 window.prompt 调用点

**Files:**
- Modify: `web-frontend/src/components/task/TaskRuntimePanel.tsx`（约 1426 的 window.prompt 编辑计划处）

- [ ] **Step 1: 定位并替换 window.prompt**

Run: `cd web-frontend && grep -n "window.prompt" src/components/task/TaskRuntimePanel.tsx`

在组件状态区加：
```tsx
const [editingPlan, setEditingPlan] = useState<PlanTask[] | null>(null);
```

把 `window.prompt(...)` 调用替换为 `setEditingPlan(currentTasks)`，并在 JSX 末尾渲染：
```tsx
{editingPlan && (
  <PlanEditor
    initialTasks={editingPlan}
    onSave={async (json) => { /* 原 prompt 后的保存逻辑，json 即新计划字符串 */ }}
    onClose={() => setEditingPlan(null)}
  />
)}
```

- [ ] **Step 2: 类型检查 + 手动验证**

Run: `npx tsc -b --pretty`
手动：触发编辑计划，确认弹出模态编辑器（表单/JSON 双模式），保存生效，不再弹浏览器原生 prompt。

- [ ] **Step 3: Commit**

```bash
git add web-frontend/src/components/task/TaskRuntimePanel.tsx
git commit -m "feat(ui): replace window.prompt with PlanEditor modal"
```

---

## 阶段 F：打磨

### Task F1: 弱化 deriveRouteExplanation 推断文案

**Files:**
- Modify: `web-frontend/src/components/task/TaskRuntimePanel.tsx:160-214`（deriveRouteExplanation）

- [ ] **Step 1: 推断文案折叠或弱化样式**

当路由说明含"实时 plan_ready 路由事件不可用"等推断标记时，默认折叠为单行摘要（如"路由：只读并行委派（点击展开推断说明）"），点击展开看完整推断。

修改渲染路由决策卡片处：
```tsx
<RuntimeStoryCard title="路由决策" defaultCollapsed={true}>
  {isInferred && <div className="text-xs text-muted mb-2">⚠ 基于运行记录推断，非实时事件</div>}
  <MarkdownContent content={routeExplanation} />
</RuntimeStoryCard>
```

- [ ] **Step 2: 类型检查 + 手动验证**

Run: `npx tsc -b --pretty`
手动：确认路由决策卡片默认折叠，有推断提示标记。

- [ ] **Step 3: Commit**

```bash
git add web-frontend/src/components/task/TaskRuntimePanel.tsx
git commit -m "ui: collapse inferred route explanation by default"
```

---

### Task F2: 长结果全屏查看抽屉

**Files:**
- Create: `web-frontend/src/components/task/ResultFullView.tsx`
- Modify: `web-frontend/src/components/task/TaskRuntimePanel.tsx`（最终结果卡片加"全屏"按钮）

- [ ] **Step 1: 新建 ResultFullView 抽屉**

```tsx
interface ResultFullViewProps {
  content: string;
  onClose: () => void;
}

export function ResultFullView({ content, onClose }: ResultFullViewProps) {
  return (
    <div className="fixed inset-0 z-50 bg-black/40 flex items-center justify-center" onClick={onClose}>
      <div
        className="w-[900px] max-w-[95vw] h-[85vh] rounded-lg bg-background border border-border flex flex-col"
        onClick={e => e.stopPropagation()}
      >
        <header className="flex justify-between items-center px-4 py-3 border-b">
          <h2 className="font-medium">最终任务结果</h2>
          <button onClick={onClose} className="text-muted">✕</button>
        </header>
        <div className="flex-1 overflow-y-auto p-4">
          <MarkdownContent content={content} />
        </div>
      </div>
    </div>
  );
}
```

- [ ] **Step 2: 最终结果卡片加全屏按钮**

在最终结果 `RuntimeStoryCard` 标题区右侧加按钮：
```tsx
<button onClick={() => setShowFullResult(true)} className="text-xs text-muted">全屏查看</button>
```
末尾渲染：
```tsx
{showFullResult && <ResultFullView content={finalResult} onClose={() => setShowFullResult(false)} />}
```

- [ ] **Step 3: 类型检查 + 手动验证**

Run: `npx tsc -b --pretty`
手动：点全屏查看，确认抽屉打开、markdown 完整渲染、可滚动、点遮罩关闭。

- [ ] **Step 4: Commit**

```bash
git add web-frontend/src/components/task/ResultFullView.tsx web-frontend/src/components/task/TaskRuntimePanel.tsx
git commit -m "feat(ui): add full-screen view for long final results"
```

---

## 验收清单（全部完成后跑一遍）

- [ ] **问题1**：主区无"任务执行"大盒子，各阶段为独立卡片，与消息流同级，过程卡片可折叠
- [ ] **问题2**：worker 结果区渲染 markdown（含表格/代码块/列表），可滚动
- [ ] **问题3**：最终任务结果区渲染 markdown，可滚动，可全屏查看
- [ ] **问题4**：Token/Cache 卡仅侧栏一份，主区和 worker 卡内无重复
- [ ] **问题5**：只读并行委派任务后，Token 卡显示真实数值（非全 0、model 非 unknown），无"provider usage 缺失"假诊断
- [ ] **额外**：编辑计划弹模态编辑器，非浏览器 prompt；路由推断说明默认折叠
- [ ] **回归**：`cargo build` 全 workspace 通过；`cargo test` 既有测试全 pass；`npx tsc -b` 无错误；常规单 agent 对话不受影响
