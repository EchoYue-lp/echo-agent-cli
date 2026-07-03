# Subagent 统一重构:消灭 Worker 概念(Durable Plan)

> **跨上下文执行文档**。本文件是「Task + Subagent 二元化、Worker 从领域/协议/UI 消失」这件事的单一事实源。
> 新窗口读本文件 + `docs/system-deep-dive/03-subagent.md` 即可恢复全局,不靠会话记忆。
> **创建**:2026-07-02。**状态**:方案已定稿(经三方 review 整合 + 代码核实),待执行。

---

## 0. 一句话

**产品模型只保留 Task 和 Subagent:Task 负责「目标拆解与状态流转」(Ought to do),Subagent 负责「专业角色与执行」(How to do)。Worker 不再是一等概念,从领域模型、事件协议、前端 store、UI 文案中彻底消失,仅保留为内部实现词(tokio task / semaphore / pool)。**

执行铁律:**先让 subagent 事件能独立承载全量执行流 + 稳定 execution_id,再删 worker 协议和 bridge** —— 不能先删后建(否则会丢全部 thinking/tool/token 执行流,见 §3 P0 #1)。

---

## 1. 背景与动机

### 1.1 现状:三个概念,实际是「一次派发的两个翻译视角」

EKO 运行时同时存在三个概念:

| 概念 | 是什么 | UI 位置 | 数据源 |
|---|---|---|---|
| **Task** | plan 里的一个 todo(`PlanTask`/`TodoItem`),带状态机 | 右栏「任务列表」 | `taskRuntimeStore` |
| **Subagent** | 被 `.md` 声明、启动时注册的角色化子 agent(explorer/reviewer/...) | 右栏「Subagents」面板 | `subagentStore`(`subagent://event`) |
| **Worker** | subagent 派发出去的运行实例(thinking/tool/token 流) | 聊天流 inline + 「Worker 状态」 | `workerTraceStore`(`worker://trace`) |

调查发现:**Subagent 和 Worker 是同一次派发的两个视角**,不是两个独立物种。`SubagentCard.tsx:67` 点开卡片调的是 `selectWorker()`;`WorkerStreamBlock.tsx:144` 的 aria-label 写的是「subagent」。命名已经精神分裂。

### 1.2 为什么 Worker 是冗余的(三证据)

**证据 A:存在一个 350 行的「翻译桥」,把 `SubagentEvent` 一对一翻成 `WorkerTraceEvent`**
`src/tauri/mod.rs:373-726` 整整 350 行,就是 `match ev { DispatchThinkingDelta => emit WorkerThinkingDelta, ... }`。如果两个概念真有独立价值,不会是纯翻译关系。**翻译桥的存在 = 概念冗余的化石。**

**证据 B:`SubagentEvent` 已经是 `WorkerTraceEvent` 的能力超集**
对照 13 vs 21 个变体,subagent 派发相关的 11 个一一对应(thinking/tool/token/started/completed/failed/cancelled 全有),Worker 唯一多出来的是 `Artifact`(产物),补一个变体即可。所以合并是「删冗余翻译」,不是「扩 subagent 能力」。

**证据 C:业界根本没有这个概念**
Claude Code 的 Task tool 派发子 agent,执行流(thinking / tool_use / tool_result / token)就是那个子 agent 自己的事件流,不存在第二套「worker 事件总线」再做一次翻译。Codex / Cursor / Devin 同理。EKO 这套 Worker 是自创的额外抽象,成熟系统都没收敛出这种东西 = 它不是被普遍需要的。

> 参考实现:Claude Code [Subagents in the SDK](https://code.claude.com/docs/en/agent-sdk/subagents)、[Stream responses in real-time](https://code.claude.com/docs/en/agent-sdk/streaming-output)。

---

## 2. 目标与非目标

### 目标
1. **领域模型**:`TaskRun → PlanTask[] → SubagentRun[]`(1 Task : N SubagentRun,模型预留 Vec,当前实现 1:1)。
2. **事件协议**:单一 `execution://event` 通道,payload 带 `kind: "run"|"task"|"subagent"` 区分。
3. **稳定 identity**:`execution_id`(`{task_id}:{attempt}`)在派发点生成、随事件透传,不再由 bridge 临时分配。
4. **前端**:单一 `subagentRunStore`,Task 嵌套 Subagent 的父子树 UI。
5. **消灭**:Rust 侧 `WorkerTraceEvent`/`WorkerTraceEventKind`/`worker://trace`/`WorkerTraceSink` + 前端 `workerTraceStore`/`WorkerStreamBlock`/`WorkerDetailView` + 文档 `worker-runtime-redesign-plan.md`。

### 非目标(本次不碰)
- `AgentRole::Worker`(框架 `config.rs`)—— 框架 TaskExecutor 内部角色枚举,保留。
- `AgentPool`(`agent_pool.rs`)—— 多会话 ReactAgent 复用池,保留。
- `scheduler/`(cron 定时)—— 定时任务,保留。
- `ManagerWorkerOrchestrator`(框架 team 调度器)—— team 子系统,保留。
- `TaskWorker` trait / `RealTaskWorker` / `ScriptedWorker` —— **保留命名**(内部调度抽象,改名牵连 `run_dag<W: TaskWorker>` 泛型 + 测试,收益低;它们是「worker pool / dispatcher」的内部实现词,符合「worker 仅保留为内部实现词」的目标)。
- run_code 真沙箱注入、KnowledgeMismatch 反哺 —— 见 MASTER-PLAN §五 剩余快项,与本次无关。

---

## 3. 设计原则(经 review 纠偏)

### P0 #1:不能先删翻译桥
`src/tauri/mod.rs:595/617/641/663/687/713` 的 thinking/token/tool 分支**全是 `continue`**,只 emit `worker://trace`,**根本不走 `subagent://event`**(行 717 只有 started/completed/failed/cancelled 4 个生命周期事件走到)。

**直接删桥 = 前端丢失全部执行过程**(thinking 流、tool 调用、token 增量),UI 上 subagent 只剩「启动了/结束了」两个空壳。

→ 正确顺序:**先让 `subagent://`(或统一后的 `execution://event`)能承载全量 started/thinking/tool/token/usage/completed/failed,再删 worker bridge**。

### P0 #2:`SubagentEvent` 缺稳定关联字段,这是双账本根源
当前框架事件(`echo-agent/src/agent/subagent/events.rs:14-139`)只有 `parent + agent`,**没有 `run_id / task_id / subagent_run_id`**。`src/tauri/mod.rs:379-431` 靠本地 `HashMap<(parent, agent), VecDeque>` + 自增 `next_dispatch_seq` 临时分配 dispatch id。

**后果**:一个 Task 派多个同名 `explorer` 并发时,无法可靠归属到具体 Task。

→ 修复路径已现成:`echo-core/src/tools/mod.rs:603` 的 `ExternalRunContext.execution_id` 字段(注释行 607-609 明确预留给此用途),当前 EKO 在 `react/mod.rs:2154` 写死 `None`。**填值即可,基础设施不用新建。**

### P1 #3:`PlanTask.executions` 不直接加到 plan artifact
`PlanTask` 是 planner 产物,描述「要做什么」。`executions` 是运行时结果,塞回 `PlanTask` 会污染 plan artifact,让 plan 编辑/重排/重试更复杂。

→ Task → SubagentRun 的关联**通过 `SubagentRun.task_id` 查询/投影**得到,`PlanTask` 不持有 executions。

### P1 #4:`SubagentEvent` 加 `DispatchArtifact` 放框架层要谨慎
如果 artifact 是通用 subagent 执行产物 → 可放框架;如果是 EKO 的文件/diff/报告/UI artifact → 放应用层 `SubagentRunEvent`,不污染 `echo-agent` 框架。

→ 本次 **artifact 放应用层**(EKO 的产物是文件/diff/报告,产品形态依赖)。

### P1 #5:三族通道 → 单一 `execution://event`
语义上 run/task/subagent 三族没问题,但实现上不需要三个 Tauri channel —— 否则重演今天 `worker://trace + subagent://event` 并存的混乱。

→ 单通道 `execution://event`,payload 带 `kind` 区分。前端只订阅一个流。

---

## 4. 目标领域模型

### 4.1 不改的(已存在,复用)
- `TaskRun`(`echo-agent-app-core/src/tasks/task_runtime/types.rs:721`)—— 顶层 run。
- `TaskPlan`(`types.rs:751`)—— run 的结构化计划。
- `PlanTask`(`types.rs:767`)—— 计划里的一行 todo。**不加 executions 字段**(P1 #3)。
- `SubagentDefinition`(`echo-agent/src/agent/subagent/types.rs:97`,框架层)—— 角色定义。**不在应用层重建**(框架已有,复用)。

### 4.2 新增(应用层)
```rust
// echo-agent-app-core/src/tasks/task_runtime/types.rs(新增)
/// 一次 subagent 派发的运行实例。原 Worker 概念的归一化载体。
pub struct SubagentRun {
    /// 稳定执行 id,= 原 worker_id 的稳定版本。格式 "{task_id}:{attempt}"。
    pub subagent_run_id: String,
    /// 父 TaskRun。
    pub run_id: String,
    /// 父 PlanTask。Task → SubagentRun 关联靠此字段查询投影(不污染 PlanTask)。
    pub task_id: String,
    /// 角色名:explorer / reviewer / implementer / ...
    pub subagent_name: String,
    /// 重试序号(第几次派发)。
    pub attempt: u32,
    /// running / completed / failed / cancelled
    pub status: SubagentRunStatus,
    /// token / cache / duration 汇总。
    pub usage: SubagentRunUsage,
    /// 返回给 Task 的产出(成功时)。
    pub result: Option<String>,
    // events: thinking/tool 流不持久化(内存 + 实时流,重启后不恢复,与原 Worker 行为一致)
}
```

### 4.3 Task 状态聚合规则
```
所有 executions 成功            → Task completed
任一 execution 失败且无重试额度  → Task failed
还有 execution running          → Task running
```

### 4.4 关系图
```
TaskRun
  └─ PlanTask[]              // plan 生成,描述「要做什么」
       └─ (查询投影) SubagentRun[]   // 一个 task 可由一个或多个 subagent 执行
            ├─ SubagentDefinition   // 角色(框架层,复用)
            └─ events               // thinking/tool/token 流(内存,不持久化)
```

---

## 5. 目标事件协议

### 5.1 单一通道 `execution://event`
撤回原方案「三族 channel(run:// task:// subagent://)」—— 合通道,避免双 channel 并存重演。

### 5.2 payload 结构
```ts
// 统一事件,前端只订阅 execution://event 一个流,按 kind 分流
{
  kind: "run" | "task" | "subagent",
  // kind=run:    run_id, status, ...
  // kind=task:   task_id, run_id, status, ...
  // kind=subagent: 见下
}

// kind=subagent 时的 event 枚举(吸收原 WorkerTraceEventKind 的 11 个执行流变体)
subagent: {
  subagent_run_id, task_id, run_id,
  event: "started" | "thinking_delta" | "thinking_started" | "thinking_ended"
       | "token_delta" | "tool_started" | "tool_completed"
       | "usage" | "artifact"        // artifact 放应用层(P1 #4)
       | "completed" | "failed" | "cancelled",
  ...event-specific fields
}
```

### 5.3 原 Worker 变体的归属
| 原 `WorkerTraceEventKind` | 归宿 | 说明 |
|---|---|---|
| `RunStarted/Completed/Failed/Cancelled/StatusChanged` | `kind:"run"` | TaskRun 生命周期,本就不属于 subagent |
| `WorkerPlanned` | `kind:"task"` | PlanTask 调度,归 task 族 |
| `WorkerStarted/Completed/Failed/Cancelled` | `kind:"subagent" event:started/completed/failed/cancelled` | 一一对应 |
| `WorkerThinkingStart/Delta/End` | `event:thinking_started/delta/ended` | 一一对应 |
| `WorkerLlmUsage` | `event:usage` | 对应 `DispatchThinkingEnded` 的 tokens |
| `WorkerToolStart/Result` | `event:tool_started/completed` | 一一对应 |
| `WorkerTokenDelta` | `event:token_delta` | 一一对应 |
| `WorkerArtifact` | `event:artifact`(**应用层**) | 原本 Worker 独有,本次新增 |
| `ApprovalRequested/Resolved` | `approval://`(已存在) | 横切事件,不进任何族 |

### 5.4 框架层 `SubagentEvent` 需要补的字段(P0 #2)
每个变体加 `execution_id: Option<String>` + `run_id: Option<String>`(用 Option 保持框架对外向后兼容,EKO 一定填值)。框架 executor emit 时从 `req.runtime_context.execution_id` 透传。

---

## 6. 迭代路径(5 阶段,严格顺序)

> 核心原则:**先建能承载全量事件的 subagent 事件源 + 稳定 execution_id → 再让 subagent 事件覆盖全量 → 前端切源 → 最后删 worker**。任何时刻都只有一套在跑(灰度过渡)。

### 🟢 阶段 1:建立 SubagentRun + 稳定 identity(纯新增,零破坏)
> 应用层为主,框架层最小改动。worker 链路仍在跑,新旧并存。

**框架层**(`echo-agent/`):
1. `SubagentEvent` 每个变体加 `execution_id: Option<String>` + `run_id: Option<String>` 字段(`events.rs`)。
2. 框架 executor(`subagent/executor.rs`)13 处 emit 点,从 `req.runtime_context` 透传 `execution_id`/`run_id`。

**应用层**(`echo-agent-app-core/`):
3. 新增 `SubagentRun` / `SubagentRunStatus` / `SubagentRunUsage` 类型(`task_runtime/types.rs`)。
4. `TaskRuntime` 派发 subagent 时,设置 `execution_id = format!("{task_id}:{attempt}")` 写入 `ExternalRunContext.execution_id`(替换 `react/mod.rs:2154` 当前写死的 `None`)。

**验证**:`./scripts/verify-all-crates.sh`(框架逐 crate,强制)+ 前端 `tsc -b`。
**产出**:SubagentRun 模型建立、execution_id 从派发点稳定生成。worker 链路仍正常(灰度)。

### 🟡 阶段 2:让 subagent 事件成为全量事件源(双发灰度)
> 此时 worker bridge 仍在跑,新旧并存。前端先能把数据接进新 store。

**应用层**:
1. 新增 `SubagentRunEvent`,承载全量:started / thinking_* / token_delta / tool_* / usage / **artifact**(应用层,P1 #4)/ completed / failed / cancelled。
2. **关键改造**:`src/tauri/mod.rs` 的 bridge,把 thinking/tool/token 那些分支从 `continue` 改成**同时 emit `execution://event`**(kind=subagent)。worker bridge 暂时保留,双发。
3. 前端 `useTauriChat.ts` 新增监听 `execution://event`,数据接进升级后的 `subagentRunStore`。

**验证**:手动跑复杂任务,确认 `execution://event` 能完整收到 thinking/tool/token 流。
**产出**:`execution://event` 成为全量事件源,前端新 store 有数据。

### 🟠 阶段 3:前端切数据源,合并 store
> 前端从双源切到单源。**状态:后端已就绪(3a),前端组件切源待续**。

#### 后端(3a,已完成,commits `6ebc2e6`+`a5ffe7a`)
- `ExternalRunContext.message_id`(echo-core)+ `SubagentEvent::DispatchLlmUsage`(完整 cache 诊断)+ `DispatchStarted.message_id`
- bridge 双发:`started` 事件带 `message_id`,`usage` 事件带 model/cached/cache_creation/total/usage_reported
- 前端 `subagentRunStore.ts` 已升级(加 messageId/usageEvents/cache 字段)+ `workerProgress.ts` 已重写适配 ExecutionEvent(**已 stash,组件未切源时 tsc 红**)

#### 前端组件切源(3b,待续 — 新窗口接续)
**关键决策(已与用户确认)**:message_id 和 cache 字段都由后端补齐(3a 已完成),不降级、不混用旧 store。

**字段映射(WorkerTraceState → SubagentRunState)**:
| 旧 | 新 | 备注 |
|---|---|---|
| `workerId` | `subagentRunId` | store key 也变:旧 `${runId}::${workerId}` → 新 `subagentRunId`(单 id) |
| `agentName?` | `agent`(非 opt) | |
| `parentWorkerId?` | `parent?` | |
| `title?` | 无 | UI 降级:`agent \|\| subagentRunId` |
| `startedAt: string`(ISO) | `startedAt: number`(epoch ms) | 时间格式变了 |
| `completedAt?` | 无(用 durationMs) | |
| `status`(含 planned) | `status`(无 planned) | 去掉 planned |
| `messageId?` | `messageId?` | 3a 已补 |
| `events: WorkerTraceEvent[]` | `events: ExecutionEvent[]` | 类型变(见下) |

**事件映射(WorkerTraceEvent → ExecutionEvent)**:
| 旧 `event_type` | 新 `event` | 字段 `payload.*` → 顶层 |
|---|---|---|
| `worker_started` | `started` | — |
| `worker_thinking_start/delta/end` | `thinking_started/delta/usage` | `content` 顶层 |
| `worker_tool_start` | `tool_started` | `name`/`args` 顶层(非 `payload.name`) |
| `worker_tool_result` | `tool_completed` | `name`/`result`/`success` 顶层 |
| `worker_token_delta` | `token_delta` | `content` 顶层 |
| `worker_llm_usage` | `usage`(DispatchLlmUsage) | model/cached/cache_creation/total 顶层(3a 补) |
| `worker_completed/failed/cancelled` | `completed/failed/cancelled` | — |
| `worker_planned`/`run_*` | 无 | 不迁移(run 级另走 task://event) |

**待改文件清单(8 组件 + 1 hook + 1 util)**:
1. `hooks/useTauriChat.ts` — 删 `normalizeWorkerTraceEvent`(:60)+ worker://trace/subagent://event 监听(留 execution://event)。**注意**:`run_started` 副作用(:131 触发 loadByConversation)迁移到 execution 事件或保留(它来自 chat.rs 路径,不在 SubagentEvent,需另查)。
2. `utils/workerProgress.ts` — ✅ 已重写(stash 里)
3. `components/chat/WorkerStreamBlock.tsx`(268 行)— props 类型改 `SubagentRunState`/`ExecutionEvent`;`reconstructSteps`(:31)、`workerResult`(:76)重写(event 字符串 + 顶层字段);`selectWorker(runId, workerId)` 改单 id;children 过滤 `parentWorkerId`→`parent`;`worker.title||agentName` 降级
4. `components/chat/ParallelExecutionBlock.tsx`(58 行)— `useWorkerTraceStore`→`useSubagentRunStore`;`w.messageId===messageId` 现在用 `run.messageId`(3a 已补);顶层过滤 `parentWorkerId`→`parent`
5. `components/task/WorkerDetailView.tsx`(344 行)— 同 WorkerStreamBlock,`reconstructSteps`(:53,含 text/usage step);`usageLine`(:146)改读 usageEvents 的 cache 字段;`cacheUsageForWorkers` 入参类型变
6. `components/task/TaskRuntimePanel.tsx` — `cacheUsageFromEvents`(:168)/`isUsageEvent`(:145)改读 ExecutionEvent 的 `usage` 事件 + 顶层 cache 字段;`traceWorkerForTodo`(:430 按 agentName 匹配改 agent)
7. `components/subagent/SubagentCard.tsx`(189 行)— 类型 `SubagentState`→`SubagentRunState`;`s.id`→`s.subagentRunId`;`selectWorker(s.parent, s.id)` 改单 id。**最简单,机械**
8. `components/layout/RightRail.tsx`(205 行)— 删 `useWorkerTraceStore`+`useSubagentStore`(行 6-7),换 `useSubagentRunStore`;`visibleWorkers`(:57)key 变 + `startedAt` 类型变;`<SubagentPanel subagents>` 数据源换 runs
9. `components/chat/ChatPanel.tsx` — `traceWorkers["runId::workerId"]`(:35 双 key)改单 key `runs[subagentRunId]`
10. `stores/workerDetailStore.ts`(18 行)— `selected:{runId,workerId}`→`{subagentRunId}`;`selectWorker` 签名单参

**建议顺序**:先 workerDetailStore + SubagentCard(机械)→ RightRail + ChatPanel(数据源/key)→ WorkerStreamBlock + ParallelExecutionBlock(重放逻辑)→ WorkerDetailView + TaskRuntimePanel(cache)→ useTauriChat(删旧监听)。每个改完跑 `npx tsc -b` 渐进消错。

**验证**:`npx tsc -b` + `npm run build`(零 error)+ UI 手动跑复杂任务确认 thinking/tool/token 流 + Token/Cache 面板正常。
**产出**:前端单 store、单数据源,UI 无 worker。

### 🔴 阶段 4:最后才删 worker 协议和 bridge
> ⚠️ **只有确认前端已 100% 切到 `execution://event` 后才执行**。建议新鲜上下文做(AGENTS.md 高风险步骤规则)。

**应用层**:
1. 删 `src/tauri/mod.rs:373-726` 整个 bridge task(含 `worker://trace` 所有 emit + `allocate_dispatch_id`/`current_dispatch_id`/`finish_dispatch_id` 本地 HashMap)。
2. 删 `WorkerTraceEvent` / `WorkerTraceEventKind`(`task_runtime/types.rs:547-650`,~100 行)。
3. 删 `WorkerTraceSink`(`executor.rs:42`)+ `trace_sink` 参数链路(`executor.rs` ~15 处 + `task_tools.rs` 的 `CURRENT_TRACE_SINK` task_local + `with_run_context`)→ 改用应用层事件 sink。
4. 删 `chat.rs` 的 `emit_worker_trace_event`(13 处),改发 `execution://event`。
5. 删生成的 `WorkerTraceEvent.ts` / `WorkerTraceEventKind.ts`。

**保留**:`TaskWorker` trait / `RealTaskWorker` / `ScriptedWorker` 命名(内部调度抽象,非领域概念)。

**验证**:全 feature 矩阵 + 手动跑任务 + `cargo clean`。
**产出**:Rust 侧 `Worker*` 领域类型全部消失(仅保留 `TaskWorker` trait 内部调度名)。

### 🔵 阶段 5:文档
1. 删 `docs/worker-runtime-redesign-plan.md`(过时设计,AGENTS.md 允许直接删)。
2. 更新 `docs/system-deep-dive/03-subagent.md`,加「执行流事件与 execution_id」节。
3. 更新 `docs/system-deep-dive/README.md` 待跟进列表(若涉及)。

---

## 7. UI 目标:父子嵌套树

参考 Cursor(Agent Mode)和 Devin 的演进方向。把 Subagent 作为 Task 的动态子节点;原 Worker 的实时流和统计数据作为 Subagent 展开后的详情页。

```
📋 右栏:任务概览树(嵌套,渐进展开)
├─ 🟩 Task 1: 调研路由设计 (已完成)
│  └─ 🕵️ explorer · 12s · 8.9k tok  (点击 → 聚焦聊天流对应位置)
├─ 🔵 Task 2: 编写核心逻辑 (运行中)
│  ├─ 🛠️ implementer · 45s · 已结束
│  └─ 🔍 reviewer · 运行中 🟢
└─ ⏳ Task 3: 清理冗余 (等待中)

💬 聊天流:实时执行流(inline,subagent 干活时自然出现)
   ├─ Subagent reviewer 正在审查 router.rs...
   ├─ [thinking] 边界条件需覆盖空输入...
   ├─ [tool] read_file router.rs
   └─ [tool] grep "unwrap"
```

**双位置协调**:右栏树是「概览 + 导航」(点 subagent 节点滚动/聚焦到聊天流对应段),聊天流是「实时执行」(subagent 作为对话的一部分 inline 展现)。两者数据同源(合并后的 subagentRunStore),只是渲染位置不同。

**信息职责分工**:
- **Task 节点**:业务状态 + Artifacts(代码/PR/测试报告)—— 宏观视角。
- **Subagent 节点**:角色 + Thinking + Tools + Token/Cache —— 微观/Debug 视角。

---

## 8. 影响面汇总(代码核实,2026-07-02)

| 层 | 文件数 | 需改处数(估) | 性质 |
|---|---|---|---|
| Rust app-core | 5(executor.rs, types.rs, task_tools.rs, execute_plan_tool.rs, mod.rs 导出) | ~40 | 见下 |
| Rust tauri 桥接 | 3(mod.rs, commands/chat.rs, commands/task_runtime.rs) | ~30 | 大部分可删 |
| 前端 store | 3(workerTraceStore, workerDetailStore, subagentStore) | ~15 | 合并 |
| 前端组件 | 5(WorkerStreamBlock, ParallelExecutionBlock, WorkerDetailView, SubagentCard, RightRail) + 2 hook | ~12 | 重构 |
| 生成代码 | 2(WorkerTraceEvent.ts, WorkerTraceEventKind.ts) | 2 | 删 |
| 测试 | 1(executor.rs mod tests) | ~4 | 改 mock |
| 文档 | 1(worker-runtime-redesign-plan.md) + 03-subagent.md | ~66 处 | 删/重写 |
| **合计** | **~20 个文件** | **~170 处** | |

其中 ~60% 是机械删除(翻译桥、类型定义、生成代码、文档),~40% 是实质工作(SubagentRun 模型、execution_id 透传、trace_sink 链路、UI 合并)。

### 关键代码锚(行号随提交变,以函数/类型名为准)

**Worker 事件契约(待删)**:
- `WorkerTraceEventKind` — `task_runtime/types.rs:550-572`(21 变体)
- `WorkerTraceEvent` — `task_runtime/types.rs:577-591`
- `WorkerTraceSink` — `executor.rs:42`

**翻译桥(待删)**:
- `src/tauri/mod.rs:373-726`(350 行,thinking/tool 分支 `continue` 在 :595/:617/:641/:663/:687/:713)
- 本地 HashMap 双账本 — `src/tauri/mod.rs:379-431`(`allocate_dispatch_id`/`current_dispatch_id`/`finish_dispatch_id`)

**Subagent 事件(待扩展)**:
- `SubagentEvent` — `echo-agent/src/agent/subagent/events.rs:14-139`(13 变体,只有 parent+agent,缺 run_id/task_id)
- 框架 emit 点 — `echo-agent/src/agent/subagent/executor.rs`(13 处:293/357/396/773/782/793/807/828/838/849/863...)

**稳定 identity 基础设施(现成,待填值)**:
- `ExternalRunContext.execution_id` — `echo-core/src/tools/mod.rs:603-615`(注释 :607-609 明确预留给此用途)
- 当前写死 None — `echo-agent/src/agent/react/mod.rs:2154`

**前端 store(待合并)**:
- `useWorkerTraceStore` — `stores/workerTraceStore.ts:94`(151 行,21 种 event_type)
- `useSubagentStore` — `stores/subagentStore.ts:39`(110 行,仅 4 种粗粒度)
- `useWorkerDetailStore` — `stores/workerDetailStore.ts:14`

**前端 UI(待改造)**:
- `WorkerStreamBlock` — `components/chat/WorkerStreamBlock.tsx:103`(268 行,aria-label :144 写的是 subagent)
- `WorkerDetailView` — `components/task/WorkerDetailView.tsx:155`(343 行)
- `SubagentCard` — `components/subagent/SubagentCard.tsx:37`(`:67` 点开调 `selectWorker()`)
- `RightRail` — `components/layout/RightRail.tsx`(`:35` workerStore + `:36` subagentStore 双消费)

**事件监听**:
- `useTauriChat.ts:117-119`(subagent://event)、`:122-125`(worker://trace)

---

## 9. 验证规范(每阶段结束都跑)

```bash
# echo-agent(根是 package 非 workspace,必须逐 crate)
cd echo-agent && ./scripts/verify-all-crates.sh   # fmt + 逐 crate test + clippy + feature 矩阵

# echo-agent-cli(真 workspace)
cd echo-agent-cli
cargo fmt --all -- --check          # 必须,CI 依赖
cargo check --workspace
cargo test --workspace
cargo check --no-default-features --features gui --bin echo-agent-tauri   # GUI target
cargo clippy --all-targets -- -D warnings
cargo clean                         # 释放 target/,强制

# 前端
cd echo-agent-cli/web-frontend && npx tsc -b && npm run build
```

每阶段一个 commit(`git -c commit.gpgsign=false commit`)。阶段 1/2 可独立合并(纯新增 + 灰度双发);阶段 3/4 建议一起(前端切源 + 删后端);阶段 4 删 bridge 是最高风险点,**建议新鲜上下文做**(AGENTS.md 规则 5),改完务必手动跑复杂任务验证 thinking/tool/token 流真的还在。

---

## 10. 风险评估

| 风险 | 等级 | 缓解 |
|---|---|---|
| 阶段 4 删 bridge 后执行流断(编译过、UI 不显示) | **高** | 新鲜上下文做;改完手动跑复杂任务验证;阶段 2/3 先灰度确认新通道全量 |
| `trace_sink` task_local 链路深(`executor.rs` ~15 处 + `task_tools.rs`)漏改 | **高** | 阶段 4 单独 commit;grep `CURRENT_TRACE_SINK` / `WorkerTraceSink` 全清理 |
| 框架层加字段破坏其它复用方 | 低 | `execution_id`/`run_id` 用 `Option`,向后兼容 |
| chat.rs 独立路径(13 处)漏改 | 中 | grep `emit_worker_trace_event` 全覆盖 |
| 前端两套 UI 合并取舍 | 中 | 保留 workerTraceStore 的丰富数据结构,重命名为 subagent |

---

## 11. 决策记录

1. **方向**:产品模型 Task + Subagent 二元化,Worker 从领域/协议/UI 消失(仅留内部实现词)。经用户拍板 + Gemini(产品视角)+ GPT(模型视角)+ 本地代码核实三方收敛。
2. **顺序**:先建后删(先 SubagentRun + 全量事件源 + 稳定 id,再删 worker)。原方案「先删翻译桥」被 GPT review 证伪(`continue` 分支会丢执行流),已纠正。
3. **`PlanTask` 不加 executions**:运行时结果不污染 plan artifact,靠 `SubagentRun.task_id` 投影(P1 #3)。
4. **artifact 放应用层**:EKO 产物是文件/diff/报告,产品形态依赖,不污染框架(P1 #4)。
5. **单通道 `execution://event`**:避免双 channel 并存重演,语义区分放 payload `kind`(P1 #5)。
6. **`TaskWorker` trait 保留命名**:内部调度抽象,改名牵连泛型 + 测试,收益低;它符合「worker 仅留作内部实现词」的目标。
7. **`SubagentDefinition` 复用框架层**:不在应用层重建,框架已有(`types.rs:97`)。
8. **跨仓库顺序**:框架层(`echo-agent`)加字段先于应用层合并(应用依赖框架)。

---

## 12. 状态追踪

| 阶段 | 状态 | commit | 说明 |
|---|---|---|---|
| 阶段 1:SubagentRun + 稳定 identity | ✅ 完成 | echo-agent `b17b323` + echo-agent-cli `ef08911`(2026-07-02)+ echo-agent `7e0b918`(execution_id 透传补丁,2026-07-03) | 纯新增,零破坏。框架 SubagentEvent 9 个 Dispatch* 变体加 execution_id/run_id + executor 11 处 emit/6 处调用点透传;应用层新增 SubagentRun/SubagentRunStatus/SubagentRunUsage + execute_task 生成 `{task_id}:{attempt}` + bridge 10 arm 加 `..`(GUI feature 补漏)。**execution_id 透传补丁**(`7e0b918`):阶段 1 漏了框架 `build_runtime_context` 写死 `execution_id: None` / `message_id: None`(`set_external_context` 不存这两个字段)→ 多个同名 explorer 并行时 bridge fallback `subagent_run_id="explorer:unknown"` 串到同一 store key,GUI 只显示一个卡片。修复:ReactAgent struct 加 `external_execution_id`/`external_message_id` Mutex 字段 + set/build/clear 三处读写。附带修 `evolution/patch.rs` manual_strip clippy。验证:框架 8 crate 1402 test + CLI 410 test + GUI + clippy + 前端全绿 |
| 阶段 2:全量事件源(双发灰度) | ✅ 完成 | echo-agent-cli `e6e5cb9`(2026-07-03) | 后端 bridge 10 arm 双发 execution://event(带稳定 subagent_run_id)+ 前端新增 subagentRunStore 消费。worker trace/subagent 通道保留(灰度并存)。验证:CLI test 381 + GUI target + 前端 tsc/build 全绿 |
| 阶段 3:前端切源 + 合并 store | ✅ 完成 | 后端 echo-agent `6ebc2e6` + echo-agent-cli `a5ffe7a`(3a: message_id + DispatchLlmUsage);前端 `546fe9d`(3b: 8 组件全切 subagentRunStore,2026-07-03) | 后端补 message_id + cache 诊断字段;前端 8 组件 + workerDetailStore + workerProgress 全部从 workerTraceStore/subagentStore 切到 subagentRunStore,reconstructSteps/workerResult 重写(event 字符串 + 顶层字段),cacheUsageFromEvents 重写。useTauriChat 仍保留旧监听(灰度,只写旧 store 无 UI 读,阶段 4 删)。验证:cargo test 381 + 前端 tsc 0 错 + build + prettier 全绿 |
| 阶段 4:删 worker 协议和 bridge | ✅ 完成 | `1eb512b`(4a bridge 瘦身)+ `6ab3f31`(4b chat.rs 迁移 + 前端删旧监听,2026-07-03)+ `46d2c1f`(4c 删 WorkerTrace 类型 + 改 TraceSink 签名,2026-07-03)+ `50badb5`(4c 修复:main agent 重复渲染 + trace_sink 断流,2026-07-03) | **4a**:bridge 瘦身(删 worker://trace 翻译 350 行,保留 execution://event)。**4b**:chat.rs main agent 路径迁到 execution://event(kind=\"run\" + kind=\"subagent\" subagent_run_id=\"main\"),前端删 worker://trace/subagent://event 监听 + normalizeWorkerTraceEvent。**4c**:彻底删 WorkerTraceEvent/WorkerTraceEventKind 类型 + WorkerTraceSink/trace_sink 链路。新增轻量 ExecEvent + ExecSink(Fn(ExecEvent)),executor 69 处 emit 改 emit_exec + 字符串字面量;task_tools TraceSink 改 Fn(ExecEvent);chat_driver 删 on_worker_trace/trace_sink 方法;chat.rs sink 闭包改接收 ExecEvent;task_runtime.rs 两处 worker://trace 改 execution://event(kind=run);前端删 workerTraceStore/subagentStore + 生成代码。**4c 修复**(`50badb5`):测试发现 4b 把 main agent chat-turn 事件同时发 chat://event + execution://event 导致重复渲染 → 删 agent_event_to_chat_event 里所有 main agent emit;4c 误删 scoped_with_ctx_run_id 的 ctx.trace_sink 回灌导致 execute_plan 读不到 trace_sink(has_trace_sink=false)→ 恢复回灌 + drive_chat_inner 注入 + worker_trace_sink_to_core 真实转换 + ChatSink::trace_sink() 方法;ExecEvent event 字段 &'static str→String(反序列化要求);progressSummary failed/cancelled 返回空串(修复'失败·失败'重复);ParallelExecutionBlock 过滤 subagentRunId==='main'。验证:cargo test 410 passed + GUI + clippy 零错误 + 前端 tsc/build 全绿 |
| 阶段 5:文档 | ⏳ 待执行 | — | 删旧文档 + 更新 deep-dive |

> 每阶段完成 + 提交后,更新本表 + MASTER-PLAN.md §五。
