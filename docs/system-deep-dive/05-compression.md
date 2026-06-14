# 05 · 上下文压缩

> **归属**：框架（`echo-state` crate 的 `compression` 模块 + `echo-core` 的 trait 定义）。
> **接口**：`MemorySubsystem.context: Arc<Mutex<ContextManager>>` 是入口；`run_core_loop` 的 `run_compact` phase 每轮调用 `prepare()`；产品层不直接管这块，全靠框架自动触发。

本文剖析压缩系统：`ContextManager` 的全部职责、`prepare()` 的真实触发条件（与配置项 `compress_threshold_ratio` 是两条不同路径）、三种内置压缩器、`protected_markers` 机制（含一个静默跳过陷阱）、完整压缩流程的 6 个阶段、Token 预算追踪与 EMA 校准、工具输出截断 + 大输出溢出磁盘。

---

## §1 `ContextManager` 的职责与字段

```rust,ignore
// echo-agent/echo-state/src/compression/mod.rs:284
pub struct ContextManager {
    messages:           Vec<Message>,
    compressor:         Option<Box<dyn ContextCompressor>>,
    token_limit:        usize,
    tokenizer:          Arc<dyn Tokenizer>,
    protected_markers:  Vec<String>,                    // 默认空
    max_messages:       usize,                          // 硬上限，默认 200
    budget:             Option<TokenBudget>,            // 百分比预算分配
    metrics:            CompressionMetrics,
    visibility_horizon: Option<horizon::VisibilityHorizonCompressor>,
    memory_promoter:    Option<Arc<dyn MemoryPromoter>>,
    canonical_context:  Option<CanonicalContext>,
}
```

### §1.1 关键 public API

| 方法 | 文件:行 | 用途 |
|------|---------|------|
| `builder(token_limit)` | `mod.rs:315` | 构造器入口，对应 `ContextManagerBuilder` (`L1384`) |
| `push(message)` | `mod.rs:334` | 追加；超过 `max_messages` 触发 `apply_hard_cap()` 保留 system + protected |
| `push_many` | `mod.rs:399` | 批量 push |
| `messages() -> &[Message]` | `mod.rs:404` | 只读访问 |
| `set_messages(Vec<Message>)` | `mod.rs:411` | **替换**整个 buffer —— 用于从 `AgentCheckpoint::messages_json` 反序列化恢复 |
| `update_system(new_prompt)` | `mod.rs:896` | 替换首条 `Role::System` 消息（不存在则插到头） |
| `prepare(current_query)` | `mod.rs:916+` | 主热路径：先 visibility horizon → 判断要不要压缩 → 跑主压缩器；fallback 到 SlidingWindow(40) (`L1080`) |
| `force_compress(fallback_window)` | `mod.rs:715` | 用户主动触发（`/compact` 命令、GUI 按钮） |
| `force_compress_with_focus(focus, fallback)` | `mod.rs:782` | 带焦点提示的主动压缩 |
| `add_protected_marker(marker: String)` | `mod.rs:449` | 注册保护子串 |
| `set_visibility_horizon` / `set_memory_promoter` / `set_canonical_context` | `mod.rs:529` / `:542` / `:582` | 注入辅助组件 |
| `compression_metrics() -> &CompressionMetrics` | `mod.rs:647` | 监控指标 |
| `token_breakdown(max_context)` | `mod.rs:660-707` | `/context` UI 视图：按 system/user/assistant/tool/summary/memory 分桶统计 token |

> **注意**：没有 `add_message` 方法。任何"加一条"都走 `push(...)`。

---

## §2 ⚠️ `prepare()` 的真实触发条件

```rust,ignore
// echo-state/src/compression/mod.rs:980-988
let needs_compression = if let Some(ref budget) = self.budget {
    let allocation = budget.allocate(0, 0, estimated_tokens);
    allocation.needs_compression()
} else {
    estimated_tokens > self.token_limit
};
```

`prepare()` **看的是** `estimated_tokens > token_limit`（或可选的 `TokenBudget::allocate`），**不看** `AgentConfig.compress_threshold_ratio`。

### §2.1 `compress_threshold_ratio` 是另一条 pre-think 路径

```rust,ignore
// echo-agent/src/agent/config.rs:104
pub(crate) compress_threshold_ratio: f64,    // 默认 0.2
```

它的语义是"剩余 token 比例低于 20% 时主动 pre-think 压缩"，**触发逻辑独立于 `ContextManager::prepare`**。在 `run_core_loop` 之外的某条 pre-think 路径里被消费 —— 与 `ContextManager` 解耦。

⚠️ **后果**：

- `ContextManager::prepare(...)` 完全不知道 `compress_threshold_ratio` 的存在，仅严格地 `estimated > token_limit`。
- 调高 `compress_threshold_ratio` 会让 pre-think 路径**更早**主动压缩；但若 pre-think 路径被绕过或失效，自动压缩仍会按 `prepare()` 的硬阈值触发。
- 这是当前实现，记录在 [07-cross-cutting.md §3](./07-cross-cutting.md#3-已知陷阱清单) 第 8 项，待跟进确认是合并还是显式分工。

---

## §3 三种压缩器

### §3.1 `ContextCompressor` trait

```rust,ignore
// echo-agent/echo-core/src/compression.rs:434
pub trait ContextCompressor: Send + Sync {
    fn compress(&self, input: CompressionInput)
                -> BoxFuture<'_, Result<CompressionOutput>>;
    fn name(&self) -> &'static str { "custom" }
}
```

`Box<dyn ContextCompressor>` 有 blanket impl（`L446-454`），支持嵌套堆叠。

### §3.2 `SlidingWindowCompressor`

```rust,ignore
// echo-state/src/compression/compressor/sliding_window.rs:14
pub struct SlidingWindowCompressor {
    window_size: usize,
}
```

策略（`L41-79`）：
- 系统消息（`Role::System`）**全部保留**。
- 非系统消息：若数量 ≤ `window_size`，原样返回；否则丢弃最早 `(len - window_size)` 条，保留最近 `window_size` 条。
- `name() = "SlidingWindow"`（`L26`）。

它是 `prepare()` 失败时的 fallback（`mod.rs:1080`，固定 `SlidingWindowCompressor::new(40)`）。

### §3.3 `SummaryCompressor`

```rust,ignore
// echo-state/src/compression/compressor/summary.rs:162
pub struct SummaryCompressor {
    llm:         Arc<dyn LlmClient>,
    prompt_fn:   SummaryPromptFn,
    keep_recent: usize,
}
```

`compress` (`L278+`)：先尝试结构化 JSON 摘要，失败则降级自由文本摘要。输出 = 一条带摘要的 system 消息 + 最近 `keep_recent` 条原始消息。

`name() = "Summary"`（`L274`）。

### §3.4 `IncrementalSummaryCompressor`

```rust,ignore
// echo-state/src/compression/compressor/summary.rs:407
pub struct IncrementalSummaryCompressor { /* ... */ }
```

按字段增量合并 `StructuredSummary`，避免每次都重摘整段历史。`name()` 在 `L580`。适合长对话频繁压缩场景。

### §3.5 `HybridCompressor`

```rust,ignore
// echo-state/src/compression/compressor/hybrid.rs:35
pub struct HybridCompressor {
    stages:        Vec<Box<dyn ContextCompressor>>,
    short_circuit: bool,
    tokenizer:     HeuristicTokenizer,
}
```

流水线：把每个 stage 的输出送给下一个。`short_circuit=true`（默认）时一旦 `current_tokens ≤ token_limit` 就跳过剩余 stage（`L66-83`）。

构造器：`HybridCompressorBuilder`（`L139`）。`name() = "Hybrid"`（`L43`）。

典型用法："先 SlidingWindow 砍掉古早消息 → 再 Summary 摘要中间段 → 短路返回"。

---

## §4 `protected_markers` 机制

### §4.1 语义：子串 contains 匹配

```rust,ignore
// echo-state/src/compression/mod.rs:456-465
fn is_protected(&self, message: &Message) -> bool {
    let content = message.content_str();
    self.protected_markers.iter().any(|m| content.contains(m))
}
```

注意是 `contains`（子串）而不是 `starts_with` 或精确匹配 —— `"<skill_content"` marker 同样匹配 `"<skill_content name=\"foo\">"` 这类带属性的开标签。

### §4.2 ⚠️ 默认空 + 生产仅注册一个 marker

`ContextManagerBuilder` 默认 `protected_markers: Vec::new()`（`mod.rs:1495`）。生产环境**唯一**的注册点：

```rust,ignore
// echo-agent/src/agent/react/capabilities.rs:589-597
self.memory.context.try_lock()
    .map(|mut ctx| ctx.add_protected_marker("<skill_content".to_string()))
    .unwrap_or_else(|| {
        warn!("Could not lock context to register protected marker; skill activations may be vulnerable to compression");
    });
```

⚠️ **`try_lock` 静默跳过陷阱**：注册发生在 skill capability 安装路径中（discover_skills 之后）；用 `try_lock` 而不是 `lock().await`。如果此刻 `ContextManager` 的锁被其他任务持有，注册会**仅打 warn 然后跳过**，导致后续 skill 内容无 marker 保护，可能被压缩。

实际后果取决于产品层调用 `discover_skills` 的时机 —— 如果在 agent 安静期调用就没问题；如果与对话并发就有概率丢失保护。记录在 [07-cross-cutting.md §3](./07-cross-cutting.md#3-已知陷阱清单) 第 7 项。

测试代码（`mod.rs:1545`）注册的 marker 是 `"<skill>"`，与生产代码的 `"<skill_content"` 不同，验证测试不能完全覆盖生产路径。

---

## §5 完整压缩流程

`prepare()` 的执行顺序（`compression/mod.rs:1008-1071`）：

```
                   ┌─ 1. visibility_horizon (pre-pass)
                   │     └ 把"远期视野外"消息预先聚合 (mod.rs:924-966)
                   │
                   ├─ 2. needs_compression? → 否就直接返回
                   │
                   ├─ 3. split_protected (mod.rs:1009)
                   │     └ 把含 protected marker 的消息抽出，记录原 index
                   │
                   ├─ 4. compressor.compress(compressible)
                   │     └ 用主压缩器跑剩下的 messages
                   │
                   ├─ 5. merge_protected (mod.rs:1026, helper L505-517)
                   │     └ 把 protected 消息按原相对位置插回
                   │
                   ├─ 6. memory_promoter.promote(evicted)
                   │     └ 被淘汰的消息抛给"L3 promoter" (mod.rs:1031-1040)
                   │       典型实现 StoreMemoryPromoter 写入 Store
                   │
                   ├─ 7. promote_and_sanitize → sanitize_tool_call_pairing (L572)
                   │     └ 修复"工具调用 / 工具结果"对的孤儿状态
                   │
                   └─ 8. reinject_canonical_context (mod.rs:594-631)
                         └ 如果压缩把 system prompt 或规则丢了，再注一次
```

### §5.1 `MemoryPromoter` —— L3 升迁机制

```rust,ignore
// echo-state/src/compression/mod.rs:34-44
pub trait MemoryPromoter: Send + Sync {
    fn promote(&self, evicted: Vec<Message>) -> BoxFuture<'_, Result<()>>;
}
```

唯一的生产实现：`StoreMemoryPromoter`（`echo-state/src/memory_promoter.rs:420`）—— 把被淘汰的消息按内容写入一个 `Store`（命名空间 by config）。这是"被压缩走的内容自动晋升到长期记忆"的接口，不是必须挂的 —— 默认 `None`。

### §5.2 工具对修复 (`sanitize_tool_call_pairing`)

`compression/mod.rs:572`。问题场景：assistant 消息含 `tool_calls` 但对应的 `tool_result` 消息被压缩掉（或反过来）。修复策略是补一条合成 `[placeholder]` 占位消息保持配对，避免下次 LLM 调用直接报"unmatched tool_call_id"错误。这些 placeholder 在 transcript 投影时被 [04-memory.md §4.2](./04-memory.md#§42-is_internal_transcript_message-过滤规则) 的过滤器排除。

### §5.3 `CanonicalContext` 重注

```rust,ignore
// echo-agent/echo-core/src/compression.rs:322
pub struct CanonicalContext { /* canonical system prompt + rules + skills */ }
```

`reinject_canonical_context`（`mod.rs:594-631`）：如果压缩把 system 头部丢了或顺序错乱，再注一次。这是兜底机制 —— 主路径上压缩器不应该丢 system，但 trait 边界没禁止。

---

## §6 触发点

### §6.1 `run_compact` phase（每轮自动）

```rust,ignore
// echo-agent/src/agent/react/run/phases/compact.rs:22-75
pub(crate) async fn run_compact(
    snap: &AgentRunSnapshot,
    context: &Arc<Mutex<ContextManager>>,
    iteration: usize,
    tx: &Sender<...>,
) -> Result<CompactOutcome> {
    snap.fire_hook(HookEvent::PreCompact, Some("auto")).await;          // L28
    snap.save_runtime_checkpoint(context, None).await;                   // L31  ← pre-compact 检查点
    let prepare_result = context.lock().await.prepare(None).await;       // L34
    if prepare_result.compressed.is_some() {
        // 发 ContextCompressed 事件
        tx.send(AgentEvent::ContextCompressed { ... });
    }
    snap.fire_hook(HookEvent::PostCompact, ...).await;                   // L62
    // 把 hook output 推回 context 当 system 消息
    Ok(CompactOutcome::Continue { messages })
}
```

每一轮都跑，但 `prepare()` 内部判断是否真要压（详见 §2）。

### §6.2 `force_compress_context`（手动触发）

```rust,ignore
// echo-agent/src/agent/react/capabilities.rs:250
pub async fn force_compress_context(&self)
    -> Result<(ForceCompressStats, Option<CompressionCheckpoint>)>
{
    self.fire_lifecycle_hook(HookEvent::PreCompact, Some("manual")).await;
    let (stats, ck) = self.memory.context.lock().await
                           .force_compress(40).await?;
    self.fire_post_compact_hook("manual", &stats).await;
    Ok((stats, ck))
}
```

设计给 GUI 按钮 / `/compact` slash command 用（doc `L246-249`）。注意：**它不是工具**（没注册到 `ToolManager`）；**也不是 Tauri command**；它是 `ReactAgent` 的方法，由产品代码直接调起。

### §6.3 `PreCompact` / `PostCompact` lifecycle hooks

`PreCompact` 在压缩开始前发；`PostCompact` 在压缩完成后发。Hook output 通过 `phases/compact.rs:63-71` 被合并回 context（注意是**新的 system 消息**形式注入）。

---

## §7 失败回退

```rust,ignore
// echo-state/src/compression/mod.rs:1080 (大致位置)
// prepare 主路径 compressor.compress(...) 失败时:
let fallback = SlidingWindowCompressor::new(40);
let output = fallback.compress(input).await?;
```

兜底逻辑：用 `SlidingWindowCompressor::new(40)` —— 不需要 LLM、不会再失败。设计意图：宁可粗暴砍，也不让对话因压缩失败而崩溃。

---

## §8 Token 预算与 EMA 校准

### §8.1 `TokenUsageTracker` —— 计数，**无费用估算**

```rust,ignore
// echo-agent/echo-core/src/tokenizer.rs:222
pub struct TokenUsageTracker {
    model_name:                String,
    total_prompt_tokens:       AtomicU64,
    total_completion_tokens:   AtomicU64,
    total_tokens:              AtomicU64,
    request_count:             AtomicU64,
}
```

方法：`record(prompt, completion, total)` (`L242`)、`record_usage(&Usage)` (`L253`)、`summary() -> UsageSummary` (`L260`)、`reset()` (`L276`)。

> **CLAUDE.md 约定**："No Cost Estimation"。`TokenUsageTracker` **不**做价格表 / 不算 USD / 不输出费用。它就是个 token 计数器。任何在产品层追加的 cost UI 都属于违反项目宪法，本套文档不会展示这种用法。

### §8.2 `CalibratedTokenizer` —— EMA 平滑校准

```rust,ignore
// echo-agent/echo-core/src/tokenizer.rs:104
pub struct CalibratedTokenizer {
    inner:        Arc<dyn Tokenizer>,
    factor_bits:  AtomicU64,    // f64 EMA-smoothed 校准因子
    sample_count: AtomicU64,
    ema_alpha:    f64,          // 默认 0.3 (L125)
}
```

`calibrate(estimated, actual)` (`L147-166`)：用真实 LLM 返回的 `usage.prompt_tokens` 校正本地启发式 tokenizer。EMA 公式：

```
new_factor = α * (actual / estimated).clamp(0.2, 5.0)
           + (1 - α) * current_factor
```

`count_tokens` 返回 `(base * factor).round()`（`L189-193`）。意义：随着调用次数增加，本地 token 估算会越来越贴近 API 真实值，使压缩预算决策更准。

clamp 边界 `[0.2, 5.0]` 防止单次极端样本（比如解析错误返回的奇怪 ratio）把 factor 拽偏太多。

---

## §9 工具输出截断 + 大输出溢出

`max_tool_output_tokens`（`config.rs:101`）的语义：单个 tool 调用的输出超过这个 token 数就截断。

```rust,ignore
// echo-agent/src/agent/react/run/execution.rs:122
pub(crate) async fn truncate_tool_output(&self, output: String) -> String {
    let Some(max_tokens) = self.config.max_tool_output_tokens else {
        return output;
    };
    // ...
}
```

逻辑分两阶段：

### §9.1 大输出溢出磁盘（`L160-188`）

如果输出超大且本机有 temp 目录可用，整段内容被写到一个 temp 文件，返回值变为 500 字符 preview + 提示：

```
[Output spilled to disk: /tmp/echo-agent-...txt (12.3MB). Use read_file to read the full output.]
```

LLM 看到提示后可以选择再调一次 `read_file` 工具读全量。

### §9.2 中等输出截断（`L194-213`）

未触发溢出时，按 token 预算精确截断，附加：

```
[... output truncated: 5234 tokens total → 1000 tokens shown ...]
```

如果预算太紧（剩余空间装不下"截断后内容 + 提示"），降级为：

```
{truncated}
[Output truncated, total {N} tokens]
```

这两种 marker 都在 `is_internal_transcript_message` 之外（它们是 tool role），但会显式被产品层标识为"系统截断标志"在 UI 中淡化显示。

---

## §10 `/context` 视图的 token 分桶

```rust,ignore
// echo-state/src/compression/mod.rs:660-707
pub fn token_breakdown(&self, max_context: usize) -> TokenBreakdown {
    /* 遍历 messages，按以下规则分桶: */
}
```

分桶规则（`L674-682`）：

- `Role::System` → `system` 桶
- `Role::User`：
  - 内容含 `[对话历史摘要]` 或 `[Conversation summary]` → `summary` 桶
  - 内容含 `[Relevant historical memories]` 或 `[相关历史记忆]` → `memory` 桶
  - 否则 → `user` 桶
- `Role::Assistant` → `assistant` 桶
- `Role::Tool` → `tool` 桶

返回 `TokenBreakdown` 用于 GUI 的 `/context` 命令展示一份"当前 token 用量构成"。

---

## §11 与其他文档的接口

- **`run_compact` phase 在主循环中的位置** → [01-runtime.md §5](./01-runtime.md#§5-phase-functionscommit-7e669f1)
- **`save_runtime_checkpoint(context, None)` 在 pre-compact 时写什么** → [02-task-planning.md §5](./02-task-planning.md#§5-runtimestatestore-检查点触发条件表)
- **`StoreMemoryPromoter` 把被压缩消息写到 Store 的什么命名空间** → [04-memory.md §2.2](./04-memory.md#§22-命名空间隔离)
- **`<skill_content>` 包装为何能挡住压缩** → [06-skills.md §4](./06-skills.md#§4-两条-skill-激活路径)
- **既有 API 参考**（compressor 用法、ContextManager builder）→ `echo-agent/docs/{en,zh}/04-compression.md`
