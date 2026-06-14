# 01 · 运行时核心

> **归属**：echo-agent（框架）。
> **接口**：产品层通过 `AgentRuntime` 持有 `ReactAgent`（直接持有或经由 `AgentPool::PooledAgent`），所有用户消息最终汇入这里描述的核心循环。

本文剖析 ReactAgent 的内部运转：单核心循环 + 双入口的统一、`execution_mutex` 的串行化语义、4 个子系统的边界、`AgentRunSnapshot` 的 Arc 组合设计、phase 函数拆分的形态、IntentRouter 的实际生效范围、Verifier/Critic 的 3 个集成点。

---

## §1 单核心循环 + 双入口

整个 echo-agent 的执行只有**一个**核心循环：

```rust,ignore
// echo-agent/src/agent/react/run/stream_channel.rs:99
impl AgentRunSnapshot {
    pub(crate) async fn run_core_loop(
        self,
        context: Arc<tokio::sync::Mutex<crate::compression::ContextManager>>,
        text: String,
        _message: Option<Message>,
        label: String,
        mode: StreamMode,
        recalled: usize,
        tx: mpsc::Sender<Result<AgentEvent>>,
    ) -> Result<()>
```

它通过 `mpsc::Sender<AgentEvent>` 推送事件，按值消费 `self`（因为要 `tokio::spawn` 进入 `'static` 任务）。两个入口都最终汇入它：

| 入口 | 文件:行 | 何时使用 |
|------|---------|----------|
| `ReactAgent::run_stream_channel` | `src/agent/react/run/stream_channel.rs:29` | 流式（GUI/TUI 实时显示 token） |
| `ReactAgent::run_react_loop` | `src/agent/react/run/react_loop.rs:708` | 非流式（CLI 一次性返回 `String`） |

两个入口都 `tokio::spawn(snapshot.run_core_loop(...))`，差异在于：

- 流式（`stream_channel.rs:43`）：`execution_mutex.lock_owned().await`，guard 移入 spawn 任务；返回 `BoxStream<'static, Result<AgentEvent>>`。
- 非流式（`react_loop.rs:710`）：`execution_mutex.lock().await` 持有到本函数结束；自建 channel，从 receiver 中收 `FinalAnswer` 后返回。

非流式路径还多做一件事：**`IntentRouter` 分类**（详见 §7）—— 流式入口**不做** intent 分类。这是当前代码事实，不是文档表述差异。

---

## §2 `execution_mutex`：串行化 + idle 探测器

```rust,ignore
// echo-agent/src/agent/react/mod.rs:115
pub struct ReactAgent {
    // ... 21 个字段，含
    execution_mutex: Arc<tokio::sync::Mutex<()>>,   // mod.rs:176
    // ...
}
```

这把锁是单 `ReactAgent` 实例的**全局执行序列化器**：任意时刻只有一个 run 正在跑核心循环。

它还有第二个用途——**作为 AgentPool 的 idle 探测器**：

```rust,ignore
// echo-agent-cli/echo-agent-app-core/src/agent_pool.rs:262-266
let evictable = candidate.handle.read(|a| {
    a.execution_mutex().try_lock().is_ok()
});
```

`AgentPool` 在容量满时按 `last_used` 排序，**对每个候选 agent 调用 `try_lock(execution_mutex)`** —— 锁不到说明它正在跑，跳过；锁到第一个就驱逐它。详见 [03-subagent.md §7](./03-subagent.md#§7-agentpool产品层).

---

## §3 4 个子系统

ReactAgent 的能力按职责拆成 4 个子系统，全部通过 `pub(crate) struct` 暴露给框架内部：

| 子系统 | 文件:行 | 拥有 |
|--------|---------|------|
| `ToolExecutionSubsystem` | `src/agent/react/subsystems/tool_exec.rs:26` | `tool_manager`、`subagent_registry`、`subagent_executor`、`task_manager`、`skill_registry`、`progressive_skill_registry`、`hook_registry`、`mcp_manager`、`sandbox_manager`、`intervention_callbacks` |
| `GuardSubsystem` | `src/agent/react/subsystems/guard.rs:14` | `guard_manager`、`audit_logger`、`circuit_breaker` |
| `MemorySubsystem` | `src/agent/react/subsystems/memory.rs:14` | `context: Arc<Mutex<ContextManager>>`、`store`（长期 KV）、`snapshot_manager`、`conversation_store`、`state_store`（`RuntimeStateStore`） |
| `ApprovalSubsystem` | `src/agent/react/subsystems/approval.rs:17` | `approval_provider`、`permission_service`、`pending_permission_rules`（皆 `cfg(human-loop)`） |

`ReactAgent` 本身的字段命名直接对应：`self.tools`（`ToolExecutionSubsystem`）、`self.guard`、`self.memory`、`self.approval`。

> **关键事实**：`state_store: Option<Arc<dyn RuntimeStateStore>>` 不是 ReactAgent 的顶层字段，而是 `MemorySubsystem` 的字段（`memory.rs:14-20`）。任何想存/取 runtime checkpoint 的代码路径都会经过 `self.memory.state_store`。

---

## §4 `AgentRunSnapshot`：O(1) Arc 克隆

`run_core_loop` 之所以能 `tokio::spawn`，靠的是把 ReactAgent 投影成一个全 Arc 的 snapshot：

```rust,ignore
// echo-agent/src/agent/snapshot.rs:178
pub struct AgentRunSnapshot {
    pub config:                    Arc<RuntimeConfig>,            // L60-86
    pub tools:                     Arc<ToolRuntime>,              // L125-136
    pub guard:                     Arc<GuardRuntime>,             // L155-159
    pub snapshot_manager:          Arc<RwLock<Option<SnapshotManager>>>,
    pub client:                    Arc<reqwest::Client>,
    pub cancel_token:              Option<CancellationToken>,
    pub recently_read_files:       Arc<Mutex<HashMap<String, Instant>>>,
    pub run_store:                 Option<Arc<dyn RunStore>>,
    pub current_run_id:            Option<String>,
    pub permission_service:        Option<Arc<PermissionService>>, // cfg human-loop
    pub token_tracker:             Arc<TokenUsageTracker>,
    pub state_store:               Option<Arc<dyn RuntimeStateStore>>,
    pub conversation_store:        Option<Arc<dyn ConversationStore>>,
    pub critic:                    Option<Arc<dyn Critic>>,
    pub tool_execution_pipeline:   Option<Arc<ToolExecutionPipeline>>,
}
```

构造点：`AgentRunSnapshot::from_agent(agent: &ReactAgent)` (`snapshot.rs:220`) —— 全部字段是 `Arc::clone`，即引用计数 +1，无深拷贝。

为什么需要这种设计？`tokio::spawn` 要求 `Future: 'static + Send`。直接 `&self` 不行（引用不是 `'static`），把 33 字段一一深拷贝代价巨大。Arc-组合让"克隆 snapshot"便宜到可忽略，同一 agent 因此能服务大量并发流而不重复分配子系统状态。

`AgentRunSnapshot` 还提供两个 checkpoint 路径上的关键方法：
- `save_runtime_checkpoint(context, blocked_reason)` — `snapshot.rs:275-327`
- `save_transcript_projection(context, mode)` — `snapshot.rs:344-410`

详细触发点见 [02-task-planning.md §5](./02-task-planning.md#§5-检查点触发条件).

---

## §5 Phase functions（commit 7e669f1）

`run_core_loop` 不再是一个庞大的 200 行函数；它已被拆成一组职责明确的 phase 函数（位于 `src/agent/react/run/phases/`）：

| Phase | 文件:行 | 角色 |
|-------|---------|------|
| `prepare_turn` | `phases/prepare.rs:22` | turn 启动一次：发 `MemoryRecalled`、审计 user 输入、跑 `UserPromptSubmit` hook、`create_execution_node(text)` 创建 `TaskNode` |
| `run_compact` | `phases/compact.rs:22` | 每轮：`PreCompact` hook → `save_runtime_checkpoint`（pre-compact 触发）→ `ContextManager::prepare(None)` → 必要时发 `ContextCompressed` 事件 → `PostCompact` hook |
| `run_think` | `phases/think.rs:24` | 每轮：on_think_start 回调/intervention → `create_llm_stream` → 流式累积 `content_buffer + tool_call_map` → 记录 token 用量 |
| `run_tools` | `phases/tools.rs:31` | 工具分支：发 `ToolBatchStart/ToolCall` → 按 `tool_needs_approval` 拆分两批 → concurrent 批 `join_all` → 工具错误触发 `save_runtime_checkpoint(..., Some("Tool error: ..."))` → final_answer 走 `verify_answer` |
| `verify_answer` / `verify_final_text` | `phases/verify.rs:21` / `phases/verify.rs:102` | Critic 评估；通过则真终结，未过则注入 `[Verifier feedback]` 系统消息继续循环 |
| `finalize_completed_run` | `phases/finalize.rs:23` | 工具分支终结（final_answer 通过） |
| `emit_final_text` | `phases/finalize.rs:119` | 文本分支终结（LLM 直接回复无工具调用） |
| `finalize_no_response` | `phases/finalize.rs:197` | LLM 啥也没产出 → `Failed` |
| `finalize_max_iterations` | `phases/finalize.rs:220` | 达到 `max_iterations` 上限 → `Failed` |

phase 间通过 outcome 枚举传递控制流（`phases/mod.rs`）：

```rust,ignore
// phases/mod.rs:37 — 跨 phase 的可变状态
pub(crate) struct LoopState {
    stop_hook_continued: bool,
    verifier_retry_count: usize,
    task_node_id: Option<String>,
}

pub(crate) enum PrepareOutcome { /* L61 */ }
pub(crate) enum CompactOutcome { /* L72 */ }
pub(crate) enum ThinkOutcome   { /* L80 */ }
pub(crate) enum IterOutcome    { /* L109 */
    Continue,        // 继续循环
    Finish { output },     // 工具分支正常终结
    FinalText { text },    // 文本分支正常终结
    NoResponse,
    Abandoned,       // 接收端 dropped (consumer 没人监听了)
}
```

`run_core_loop` 主体（`stream_channel.rs:126-211`）就是一个 `for iteration in 0..max_iterations` + `match iter_outcome { ... }` 的瘦驱动器，所有真活儿都在 phase 函数里。

---

## §6 流式短路：`yield_event_or!` 宏

phase 函数遇到下游消费者已经 drop 接收端时，需要立刻退出而不是浪费一次 LLM 调用。这通过一组宏实现：

```rust,ignore
// echo-agent/src/agent/react/run/stream_macros.rs
yield_event_or!(tx, event);            // tx.send 失败 → return Ok(())
yield_final_event_or!(tx, event);      // 同上，标记 final
try_send_or!(tx, event);               // 同上，无 await
```

它们 short-circuit **当前函数**，把"消费者 dropped"实现为 phase 函数的 `Ok(())` 提前返回。这就是为什么 `IterOutcome::Abandoned` 通常不会经过显式 match —— 它已经在事件发送的那一行 return 走了。

---

## §7 IntentRouter：仅在非流式入口生效

```rust,ignore
// echo-agent/src/intent/mod.rs:36
pub enum Intent {
    DirectAnswer    { confidence: f32 },
    SkillRequired   { skill_name: String, confidence: f32 },
    WorkflowRequired{ workflow_name: String, confidence: f32 },
    Fallback,
}
```

`IntentRouter::classify` (`mod.rs:142`) 把任何低于 `confidence_threshold`（默认 0.7）的判定降级为 `Fallback`。

三种内置分类器：

| 类型 | 文件:行 | 行为 |
|------|---------|------|
| `KeywordClassifier` | `classifier.rs:44` | 进程内零成本匹配；通过 `add_skill_keywords(name, &triggers)` 由产品层填充（来自每个 SKILL.md 的 `triggers` 字段） |
| `LlmIntentClassifier` | `classifier.rs:297` | 语义兜底，输出 JSON `{intent, skill, confidence}` |
| `ChainedClassifier` | `classifier.rs:437` | 链式：先 keyword（零成本）→ 不命中再 LLM |

调用点：

```rust,ignore
// echo-agent/src/agent/react/run/react_loop.rs:725-782
let intent = router.classify(message, &messages).await;
match intent {
    Intent::DirectAnswer { .. } => return self.direct_answer(message).await,
    Intent::SkillRequired { skill_name, .. } => {
        // 激活 skill 并把指令推入 context（详见 06-skills.md §4）
        self.tools.skill_registry.activate(&skill_name).await?;
        self.memory.context.lock().await
            .push(Message::system(content.instructions));
    }
    Intent::WorkflowRequired { .. } => { /* 当前为 TODO，落到 ReAct */ }
    Intent::Fallback => {}
}
```

> ⚠️ **重要事实：流式入口（`run_stream_channel`）不调用 `IntentRouter`**。`grep -n "intent_router" src/agent/react/run/stream_channel.rs` 零命中。这意味着：
>
> - **GUI/TUI 流式对话**：用户消息直接进 ReAct 循环，靠 LLM 自己判断要不要调用 `activate_skill` 工具。
> - **CLI 一次性 chat()/execute()**：先经 IntentRouter，命中后产物经"路径 2"（详见 [06-skills.md §4](./06-skills.md#§4-两条-skill-激活路径)）。
>
> 两条路径产物**不对称**且只有一条受压缩保护 —— 这个分歧记录在 [07-cross-cutting.md §3](./07-cross-cutting.md#3-已知陷阱清单) 第 2 项。

---

## §8 Verifier / `LlmCritic`：3 个 final_answer 集成点

```rust,ignore
// echo-agent/src/agent/critic/llm_critic.rs:18
pub struct LlmCritic {
    model:          String,
    client:         Arc<Client>,
    system_prompt:  String,
    pass_threshold: f64,    // 默认 7.0，L45
}
```

`critique(task, answer, context)` (`llm_critic.rs:116`) 用 JSON Schema 约束 LLM 输出 `{score, passed, reasoning, ...}`，最后**用 `score >= pass_threshold` 重写 `passed` 字段**（`L173`），不信任 LLM 的自评 boolean。

配置：
- `verifier_enabled: bool` — `config.rs:134`，默认 `false`（`L193`）
- `verifier_max_retries: usize` — `config.rs:138`，默认 `2`（`L195`）

集成点（"3 个 final_answer 路径"）：

| # | 触发位置 | 文件:行 |
|---|---------|---------|
| 1 | 工具分支 / concurrent 批中的 final_answer | `phases/tools.rs:153-160` |
| 2 | 工具分支 / approval 批中的 final_answer | `phases/tools.rs:205-212` |
| 3 | 文本分支（LLM 无工具调用直接出文） | `phases/verify.rs:102` `verify_final_text` |

`verify_answer` (`phases/verify.rs:21`) **fail-open 三处**：
- `verifier_enabled=false`（`L28`）
- 没配 critic（`L31`）
- critic 调用本身 error（`L88`）

任一情况返回 `true`（视为通过），保证关掉 verifier 不会卡住循环。

未通过时，注入：
```
Message::system("[Verifier feedback] Score: X/10 (min: Y). {reasoning}")
```
循环继续；同时 `state.verifier_retry_count += 1`，到达 `verifier_max_retries` 时强制放过，避免无限重试。

---

## §9 一图归纳

```
用户消息
   │
   ├── (流式入口) ─────── execution_mutex.lock_owned() ─┐
   │                                                     │
   └── (非流式) ── execution_mutex.lock() ──┐           │
                                              │           │
                                              ▼           │
                                          IntentRouter   │
                                              │           │
                                              ├─ DirectAnswer → return
                                              ├─ SkillRequired → activate (Path 2)
                                              ├─ WorkflowRequired (TODO) → fall through
                                              └─ Fallback → fall through
                                              │           │
                                              ▼           ▼
                                       ┌──────────────────────────┐
                                       │ AgentRunSnapshot::       │
                                       │   run_core_loop          │
                                       │ ┌──────────────────────┐ │
                                       │ │ prepare_turn         │ │
                                       │ │  └ create TaskNode   │ │
                                       │ ├──────────────────────┤ │
                                       │ │ for iter in 0..max:  │ │
                                       │ │   run_compact        │ │
                                       │ │     └ save_ckpt      │ │
                                       │ │     └ prepare()      │ │
                                       │ │   run_think          │ │
                                       │ │     └ LLM stream     │ │
                                       │ │   match outcome:     │ │
                                       │ │     ├ tool_calls →   │ │
                                       │ │     │   run_tools    │ │
                                       │ │     │     └ verify   │ │
                                       │ │     │   Continue/    │ │
                                       │ │     │   Finish       │ │
                                       │ │     ├ text →         │ │
                                       │ │     │   verify_text  │ │
                                       │ │     │   FinalText    │ │
                                       │ │     └ none →         │ │
                                       │ │       NoResponse     │ │
                                       │ ├──────────────────────┤ │
                                       │ │ finalize_*           │ │
                                       │ │  └ TaskNode→Success/ │ │
                                       │ │    Failed            │ │
                                       │ │  └ save transcript   │ │
                                       │ │  └ FinalAnswer event │ │
                                       │ └──────────────────────┘ │
                                       └──────────────────────────┘
                                              │
                                              ▼
                                          AgentEvent stream → tx
```

---

## §10 与其他文档的接口

- **TaskNode、save_runtime_checkpoint、save_transcript_projection 的细节** → [02-task-planning.md](./02-task-planning.md)
- **AgentRunSnapshot 中各 store 字段的语义** → [04-memory.md §1–§4](./04-memory.md)
- **`run_compact` 内部如何决定要不要压缩** → [05-compression.md §2 / §5](./05-compression.md)
- **`SkillRequired` 路径产物为何不受压缩保护** → [06-skills.md §4](./06-skills.md#§4-两条-skill-激活路径)
- **`AgentPool` 怎么用 `try_lock(execution_mutex)` 探测 idle** → [03-subagent.md §7](./03-subagent.md#§7-agentpool产品层)
- **既有 API 参考** → `echo-agent/docs/{en,zh}/01-react-agent.md`
