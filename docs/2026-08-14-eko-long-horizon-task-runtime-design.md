# EKO 长程任务运行时完整设计

> 日期：2026-08-14
> 状态：实现前架构方案
> 基线：`echo-agent-cli@d01b653dddb4eb8ce9ff1566677bf4742166aabe`
> 配套取证：[`Codex Goal、Turn 与压缩长程运行机制取证`](./2026-08-14-codex-goal-turn-compaction-forensics.md)

## 1. 决策摘要

EKO 应实现与 Codex Goal 等价的长程能力，但**不复制 Codex 的 Goal 领域模型**。
EKO 已有的 `TaskRun` 就是“一次用户目标”，已经拥有 file-backed authority、
revisioned `TaskPlan`、DAG executor、Subagent、取消、恢复、事件和 UI 投影。
再建 `Goal`、`GoalStore` 或第二套 executor 会制造两个目标权威。

目标架构是：

```text
Conversation
└── TaskRun                     # 唯一 Goal 权威
    ├── TaskRun.goal            # 原始目标或带 hash 的输入 artifact 引用
    ├── RunContinuationState    # events.jsonl 折叠到 run-state.json
    ├── RunTurn 1               # 一次 drive_chat 执行尝试
    │   └── N 次模型上下文压缩
    ├── RunTurn 2
    │   └── ...
    └── revisioned TaskPlan
        └── PlanTask
            └── SubagentRun
```

核心实现是一个 EKO 应用层的 `TaskContinuationRuntime`：当同一 `TaskRun` 仍为
`Running`、当前没有活跃 Turn、没有 continuation deferral、没有预算或人工等待
条件时，它通过统一 `drive_chat` 启动下一个内部 continuation Turn。

必须保持以下原则：

- `TaskRun` 是 Goal，不新增 Goal store、Goal 状态机或 Goal CRUD。
- `RunTurn` 是执行尝试，不是另一个 Task，不拥有 Plan 或 DAG。
- `TaskPlan` 仍只是可修订 artifact；`TodoItem` 仍只是 UI 投影。
- `task_create/task_update/task_list/task_execute` 仍是唯一任务关系 API。
- `events.jsonl` 仍是恢复权威；`run-state.json` 和 `plan.json` 是投影。
- 所有交互面继续经过同一 `drive_chat` 和同一 TaskRuntime service。
- EKO 继续只使用文件/内存持久化，不引入或启用 SQLite。
- GUI、TUI、CLI、channel 在 Goal 创建、暂停、继续、取消、编辑和进度展示上
  功能对等。
- 产品与代码只使用 Subagent 术语。

这不是一个“大状态机”项目。`TaskRunStatus` 保持现有 6 个状态；等待用户、启动
恢复、用量限制、预算耗尽和连续受阻作为结构化 pause reason 或控制投影，不扩张
为十几个生命周期状态。

## 2. 用户体验目标

用户可以给 EKO 一个需要数小时、数十个模型窗口、多个 Turn 和多个 Subagent
才能完成的目标，然后离开。EKO 应做到：

1. 目标不会因为单 Turn 结束或反复压缩而被缩小。
2. 每一轮都根据当前工作区和 TaskRuntime 权威状态继续，而不是依赖旧摘要猜测。
3. 单 Turn 结束但 TaskRun 未完成时，自动启动下一 Turn。
4. 应用重启后能恢复，不重复执行已完成的 PlanTask，也不会对不确定副作用盲目
   重放。
5. 用户可以随时查看、暂停、继续、取消或补充要求。
6. token、耗时、Turn 数、压缩数、任务完成度和受阻原因可观测。
7. 只有完成证据覆盖原始目标和所有必需任务后，TaskRun 才进入 `Completed`。
8. 遇到错误时保留可恢复状态；只有明确不可恢复的系统错误才进入 `Failed`。

### 2.1 非目标

- 不追求一个无限长的模型上下文。
- 不把 Plan approval 编进 `TaskRunStatus`。
- 不创建第二套 Plan/Todo/Task store 或调度循环。
- 不要求每个普通 Chat/Auto 消息都自动升级为长程 TaskRun。
- 不用 transcript 充当执行状态数据库。
- 不为本地交互式终端、文件选择器或 MCP 增加错误的权限模式门控。
- 不在第一阶段下沉 EKO 产品策略到通用 `echo-agent` 框架。

## 3. 业界实现调研与取舍

### 3.1 OpenAI Codex

Codex 官方源码提供了本方案最直接的参考：

- Thread Goal 独立持久化 objective、status、预算、token 和耗时。
- Goal active 且 Thread idle 时，运行时重新读取 Goal 并
  `start_turn_if_idle`。
- continuation 使用内部 context fragment，不伪装成可见用户输入。
- completion 和 blocked 需要显式工具更新；单 Turn 结束不是 Goal 完成。
- 同一 Turn 可以经历多次 compact；下一自动 Turn 再从持久 Goal 注入完整约束。

EKO 采用它的“持久目标 + 有限 Turn + idle continuation + 完成审计”机制，但不
复制 `ThreadGoal`，因为 EKO 已有语义等价且更丰富的 `TaskRun`。

### 3.2 Cursor

Cursor 的官方 long-running agent 说明指出，长任务的主要失败模式是偏离大局、
忘记当前工作和停在部分完成；其 harness 使用执行前计划以及多个 Agent 交叉检查
来提高 follow-through。公开案例包括约一万行变更和持续数十小时的任务。

Cursor 的 cloud agent 工程总结又强调：

- 长程 Agent 需要 durable execution。
- 比“永不结束的单 workflow”更可维护的是多个完成单一任务的较短 workflow。
- Agent loop、machine state 和 conversation state 应解耦。
- append-only conversation/state 流需要处理重试，不能因重试重复投影。

这与 Codex 的 Goal/Turn 分层收敛到同一模式。EKO 因此选择多个有限 RunTurn，
而不是让一个 `drive_chat` future 永远存活。

### 3.3 Claude Code

Claude Code 官方公开 changelog 持续展示以下能力组合：session
`--resume/--continue`、自动压缩、反复压缩后的历史恢复、被中断 Turn 的恢复、
后台 Agent/任务和 follow-up Turn。公开仓库不包含其完整内部运行时，不能据此
杜撰具体数据结构；但这些外部行为同样证明“可恢复 session + 压缩 + 分段执行”
是成熟实现的共同方向。

Claude Code 的 Plan mode 也提醒我们：Plan 是可生成、可审阅的行为 artifact，
不应把 Planning/AwaitingApproval/Ready 等交互阶段扩张成复杂运行状态机。

### 3.4 LangGraph

LangGraph 官方 persistence 文档把 thread-scoped checkpoint 与跨 thread store
区分开，并把 checkpoint 用于中断恢复、HITL 和 fault tolerance。对 EKO 的启示
是：执行恢复状态应跟随 TaskRun/Turn checkpoint，用户偏好和长期记忆则继续放在
现有 memory 层；不能用长期 memory 替代精确 TaskRuntime checkpoint。

### 3.5 跨系统共性

四组实现/资料收敛出六条强信号：

1. 长任务必须有独立于短期上下文的持久目标或任务状态。
2. 执行应拆成可终止、可记账、可重试的有限单元。
3. Plan 是可修订 artifact，不是生命周期状态机。
4. 压缩只解决上下文容量，不解决任务完成性。
5. 当前工作区、checkpoint、测试和 artifact 比模型记忆更权威。
6. 自动化必须有幂等 identity、start-if-idle 和明确人工暂停边界。

## 4. 实现前门禁：已有能力与缺口

本方案在 `echo-agent` 和 `echo-agent-cli` 全仓搜索了 Goal、TaskRun、Turn、Plan、
runtime store、projection、compression、recovery、usage、unattended run 和各交互
入口。结论如下。

### 4.1 已经存在并必须复用

| 能力 | 当前权威路径 | 结论 |
|---|---|---|
| 用户目标 | `TaskRun.goal` | 直接作为 Goal objective |
| Run 生命周期 | `TaskRunStatus` | 保留精简状态，不重建 |
| 持久化 | `TaskRuntimeStore` / `FileTaskShadow` | `events.jsonl` 权威，继续扩展事件折叠 |
| Plan artifact | revisioned `TaskPlan` / `plan.json` | 不新增 plan API/store |
| DAG 执行 | framework `RuntimeDagExecutor` + EKO adapter | 不新增 continuation DAG loop |
| Subagent | `TaskRun -> PlanTask -> SubagentRun` | 继续复用 claim/attempt/result/recovery |
| 统一交互驱动 | `echo-agent-app-core/src/chat_driver.rs::drive_chat` | continuation 也必须走这里 |
| 模型边界投影 | `PreModelContextProjector` | 已在每次 model prepare 前刷新 |
| TaskRuntime 投影 | `TaskRuntimeContextProjector` | 扩展为稳定 Goal contract + 动态 capsule |
| 压缩 | framework `ContextManager` / compressors | 不在 EKO 重写压缩器 |
| 规范上下文 | framework `CanonicalContext` | 继续注入 AGENTS/project/skills |
| 未完成恢复 | `TaskRuntimeStore::recover_incomplete` | 扩展 continuation 恢复语义 |
| 后台运行 | `drive_agent_run` / `launch_unattended_run` | 归一到同一 Turn outcome/continuation 契约 |
| 前台互斥 | `SessionState.active_chat_turns` | 提升为 start-if-idle 原语的一部分 |
| 用量事件 | `AgentEvent::LlmUsage` | 从只观察扩展为持久 Run 记账 |
| 压缩事件 | `AgentEvent::ContextCompressed` | 累计到 RunTurn/TaskRun 投影 |

### 4.2 当前真实缺口

1. `drive_chat` 把 `root_message_id` 同时当作 Turn identity，并从它派生
   `formal_run_id`；跨 Turn continuation 无法绑定回已有 TaskRun。
2. `drive_chat` 返回 `Result<(), String>`，用量、压缩次数、耗时、最终回答和终止
   原因只流向 sink，没有结构化 Turn outcome 供运行时记账。
3. `finalize_task_mode_run` 在一个流结束时，只要 TaskRun 仍 `Running` 就把它改成
   `Failed`。这直接把“Turn 结束”错误等同于“Goal 失败”。
4. `TaskRuntimeContextProjector` 虽然每个模型边界都会运行，但无 Plan/Todo 时返回
   空投影；长程 Goal 在计划物化前没有稳定保护。
5. 当前 recovery capsule 把 goal 截短为 420 字符，适合进度卡片，不足以单独
   承担原始长程契约。
6. `recover_incomplete` 会把进程中断的 Running run 变成 Paused，但没有区分普通
   boot recovery 与用户明确启用的自动 continuation 策略。
7. 主 Agent 的 usage 目前用于 webhook/UI 累加，未幂等写入 TaskRun。
8. 当前已有 unattended 单 Agent loop，但没有“本轮结束、Run 仍未完成、启动下一
   RunTurn”的统一控制层。

当前源码入口索引：

- [`TaskRun`、`TaskPlan`、`RunStateSnapshot`](../echo-agent-app-core/src/tasks/task_runtime/types.rs)
- [统一 `drive_chat` 与当前单 Turn finalizer](../echo-agent-app-core/src/chat_driver.rs)
- [TaskRuntime model-boundary projector](../echo-agent-app-core/src/tasks/task_runtime/compact_context.rs)
- [file-backed read projection](../echo-agent-app-core/src/tasks/task_runtime/file_store.rs)
- [boot recovery 与 store authority](../echo-agent-app-core/src/tasks/task_runtime/store.rs)
- [unattended Agent run driver](../echo-agent-app-core/src/tasks/task_runtime/executor.rs)
- [per-conversation active Turn registry](../echo-agent-app-core/src/state.rs)
- [统一 Turn 输入规范化与大文本 artifact](../echo-agent-app-core/src/prepared_turn.rs)

### 4.3 分层判定

| 分类 | 归属 | 内容 |
|---|---|---|
| 通用机制 | `echo-agent` | 已有 model-boundary projector、compression、canonical context、conversation/runtime file store、取消和 Agent invocation；第一阶段无需新增 Goal 概念 |
| EKO 产品策略 | `echo-agent-cli` | TaskRun continuation、预算、pause reason、Goal UI、worktree/TaskRuntime completion、自动恢复策略、各交互面投影 |
| 适配边界 | `echo-agent-app-core` | run-bound Turn identity、`ChatTurnOutcome`、内部 continuation input、TaskRuntime context projection、事件转换 |

默认不修改框架。只有实现中证明某个缺口对所有 `echo-agent` 复用方都成立，且现有
通用 primitive 无法表达时，才单独提出框架 API 变更。

## 5. 领域模型与不变量

### 5.1 唯一层级

```text
Conversation 1 ── N TaskRun
TaskRun      1 ── N RunTurn
TaskRun      1 ── 0..1 current TaskPlan revision
TaskPlan     1 ── N PlanTask
PlanTask     1 ── N SubagentRun attempt
RunTurn      1 ── N context compression window
```

`RunTurn` 不是 TaskRun 的子任务；它只是主 Agent 为同一个 Goal 做的一次执行
尝试。真正可调度的工作关系仍只存在于 revisioned PlanTask DAG。

### 5.2 核心不变量

1. 一个 conversation 最多有一个 Running/Paused 的前台长程 TaskRun。
2. 一个 TaskRun 同时最多有一个 active RunTurn。
3. RunTurn identity 在 TaskRun 内单调分配，不从 root message 重新派生 Run。
4. `TaskRun.goal` 在用户未明确修改前保持不变。
5. Plan revision 可以变化，但必须保持 `TaskPlan.goal` 与 TaskRun 当前 Goal revision
   一致。
6. 单 Turn 的 `FinalAnswer` 或 stream EOF 不得直接证明 TaskRun 完成。
7. TaskRun 只有在 TaskRuntime completion gate 通过后才能 `Completed`。
8. 每个 usage delta 只能按稳定 event identity 记账一次。
9. 自动续跑前必须重新读取 file-backed run-state，而不是相信内存快照。
10. 所有 continuation 控制都经过同一 per-run claim/start-if-idle 原语。
11. 自动 continuation 不写成可见用户消息，不污染 transcript。
12. crash 后不确定的写副作用保持 Blocked/Paused，绝不盲目重放。

## 6. 持久数据设计

### 6.1 不增加新 store

继续使用现有布局：

```text
.eko/runtime/<run_id>/
├── events.jsonl       # 唯一恢复权威，append-only
├── plan.json          # 当前 TaskPlan revision 投影
└── run-state.json     # TaskRun、task execution、continuation 投影
```

不创建 `goals.json`、`turns.db`、SQLite 表或独立 continuation store。

### 6.2 RunContinuationState

在现有 `RunStateSnapshot` 中增加由 `events.jsonl` 可重建的控制投影：

```rust
pub struct RunStateSnapshot {
    pub run: TaskRun,
    pub tasks: Vec<EkoTaskExecution>,
    pub continuation: Option<RunContinuationState>,
}

pub struct RunContinuationState {
    pub enabled: bool,
    pub auto_resume_after_restart: bool,
    pub token_budget: Option<u64>,
    pub tokens_used: u64,
    pub time_used_seconds: u64,
    pub next_turn_ordinal: u64,
    pub active_turn: Option<RunTurnSummary>,
    pub last_turn: Option<RunTurnSummary>,
    pub pause: Option<RunPause>,
    pub blocker_audit: Option<BlockerAudit>,
    pub deferred: bool,
}
```

字段含义：

- `enabled` 由用户创建长程目标或显式把 TaskRun 切换为连续执行时设置。
- `auto_resume_after_restart` 只对显式长程任务生效，不把所有 Paused run 自动启动。
- `token_budget` 是可选资源上限；用户未要求时为 `None`。
- `tokens_used/time_used_seconds` 是所有 Goal RunTurn 的聚合值。
- `active_turn/last_turn` 是事件折叠后的有界摘要，不保存完整 transcript。
- `pause` 解释为什么 `TaskRunStatus::Paused`，而不是增加状态变体。
- `blocker_audit` 对相同阻塞 fingerprint 连续计数。
- `deferred` 表示有用户输入/控制操作应先处理，避免自动续跑抢占。

所有整数累计使用 checked 或 saturating 运算；任何用户文本截断必须使用
`chars().take()`，不得使用字节切片。

### 6.3 RunTurnSummary

```rust
pub struct RunTurnSummary {
    pub turn_id: String,
    pub ordinal: u64,
    pub origin: RunTurnOrigin,
    pub status: RunTurnStatus,
    pub started_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub elapsed_seconds: u64,
    pub compaction_count: u32,
    pub final_message_id: Option<String>,
    pub error_fingerprint: Option<String>,
}
```

建议枚举保持执行语义而非产品阶段：

```text
RunTurnOrigin = User | Continuation | Resume | Recovery
RunTurnStatus = Running | Ended | Cancelled | Failed
```

`Ended` 只表示本 Turn 正常结束，绝不等价于 `TaskRunStatus::Completed`。

### 6.4 Pause reason

保留现有 `TaskRunStatus::Paused`，使用结构化原因：

```text
User
NeedsInput
Approval
BootRecovery
UsageLimit
TokenBudget
RepeatedBlocker
IndeterminateSideEffect
ProviderUnavailable
```

`Failed` 只用于不可恢复的持久化损坏、无法重建的违反不变量状态或明确终止的
系统错误。普通 provider 波动、等待用户、预算耗尽和 boot recovery 都可恢复，
应为 Paused。

### 6.5 事件扩展

在现有 `RuntimeEventKind` 和 payload 体系内增加：

```text
run_continuation_configured
run_turn_started
run_turn_usage_accounted
run_turn_compacted
run_turn_finished
run_continuation_deferred
run_continuation_resumed
run_pause_reason_changed
run_goal_updated              # 后续支持用户显式改目标
```

事件必须带稳定 `event_id`。usage 的建议 identity：

```text
<run_id>:<turn_id>:usage:<provider_event_id>
```

store 在同一个 per-run 临界区内执行“判重、append event、更新 projection”。
重放 `events.jsonl` 应得到相同 `RunContinuationState`。

### 6.6 Goal 修改

第一版可以只允许暂停后修改 Goal。修改操作属于 TaskRuntime 同一权威事务：

1. CAS 校验当前 run revision/updated_at。
2. append `run_goal_updated`，保留旧/new hash 和用户原因。
3. 更新 `TaskRun.goal`。
4. 若已有 Plan，要求通过现有 `task_update` 提交匹配的新 revision；在 Plan 对齐前
   continuation 保持 deferred。
5. 新 RunTurn 投影 objective-updated context。

不得创建 `goal_update` 模型工具或第二套 Plan patch API。模型仍使用已有
`task_update` 修改任务图；产品 service 只更新 TaskRun 元数据。

## 7. Turn identity 与统一驱动改造

### 7.1 分离四种 identity

当前 `root_message_id` 同时承担多种职责。目标模型应明确分离：

| identity | 生命周期 | 用途 |
|---|---|---|
| `conversation_id` | 多个用户消息/TaskRun | transcript 与会话互斥 |
| `run_id` | 整个 Goal | TaskRuntime、Plan、预算、恢复 |
| `turn_id` | 一次 RunTurn | Agent invocation、取消、usage、压缩 |
| `root_message_id` | Goal 起始消息 | UI 归属和原始用户输入定位 |

新增薄的 `RunTurnBinding`：

```rust
pub struct RunTurnBinding {
    pub run_id: Option<String>,
    pub turn_id: String,
    pub root_message_id: String,
    pub origin: RunTurnOrigin,
    pub transcript_visibility: TurnVisibility,
}
```

普通 Chat 的 `run_id=None`；Task/continuation Turn 使用已有 `run_id`。只有第一次
Task Turn 在 run 不存在时创建 TaskRun，后续 Turn 不再从 `turn_id` 派生新 Run。

### 7.2 统一 Turn input

保留 `PreparedUserTurn` 作为外部用户输入规范化结果，再增加一个内部 continuation
表示，最后无损转换到共同的 `ChatTurnRequest`：

```rust
pub struct ChatTurnRequest {
    pub instruction: String,
    pub resources: Vec<InputResourceRef>,
    pub binding: RunTurnBinding,
}
```

continuation instruction 是内部上下文，不写入可见 user transcript。用户输入继续
通过 `PreparedUserTurn::build` 做大文本 spill 和 attachment 规范化。

### 7.3 结构化 ChatTurnOutcome

将 `drive_chat` 的返回值从 `Result<(), String>` 改为：

```rust
pub struct ChatTurnOutcome {
    pub turn_id: String,
    pub terminal: ChatTurnTerminal,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub compaction_count: u32,
    pub elapsed_seconds: u64,
    pub final_answer: Option<String>,
    pub final_message_id: Option<String>,
    pub error: Option<StructuredAgentError>,
}
```

sink 仍实时接收 `ChatDriverEvent`；outcome 只聚合控制面所需的有界信息。所有 GUI、
TUI、CLI 和 channel 调用点继续使用同一函数，不得另建 `drive_goal`。

### 7.4 替换 finalize_task_mode_run

当前 `finalize_task_mode_run` 必须被 `finalize_run_turn` 替换：

```text
persist RunTurnFinished + usage
  -> reload authoritative TaskRun
  -> terminal TaskRun? stop
  -> user cancel? Cancelled
  -> explicit pause/input/budget? Paused(reason)
  -> continuation enabled? keep Running and request continuation
  -> ordinary one-shot Task mode still Running? preserve existing failure policy
```

关键是：长程 TaskRun 的 stream EOF 只结束 RunTurn；没有 completion evidence 时既
不 `Completed`，也不 `Failed`。

`drive_agent_run` 和 `launch_unattended_run` 后续也返回/消费同一个
`ChatTurnOutcome` 契约，删除其平行的“drain stream 后自行猜测结果”逻辑。

## 8. Goal contract 与压缩鲁棒性

### 8.1 扩展现有 projector，不新建第二个权威投影器

`TaskRuntimeContextProjector` 已经在每个 model prepare 前读取当前 store；应让它
返回两个 replaceable protected projection：

1. `[eko_run_goal_contract]`：稳定 Goal 契约。
2. `[eko_runtime_recovery_capsule]`：动态 Plan/执行/证据摘要。

二者来源相同、职责不同。Goal contract 在 continuation enabled 后即存在，即使
还没有 Plan/Todo；recovery capsule 继续随 Plan 和 task execution 更新。

### 8.2 稳定 Goal contract

内容应包含：

- `run_id`、当前 TaskRun status 和 Goal revision/hash。
- 完整 `TaskRun.goal`，或原始 spill artifact 的 path + sha256 + 强制读取说明。
- “Turn 结束不等于 Goal 完成”的 continuation 契约。
- 当前工作区、TaskRuntime、文件和测试结果是权威证据。
- 不得把 Goal 缩小到当前 Turn 能做完的范围。
- 完成前按原始 Goal、引用 spec 和 TaskPlan 做逐项 audit。
- 只有 TaskRuntime completion gate 通过才允许完成。
- 相同阻塞 fingerprint 连续三 Turn 且无法继续推进才暂停为 RepeatedBlocker。
- token/time budget 与剩余量。

当前 420 字符 goal 截断不能用于这个稳定契约。短目标完整内联；长目标使用现有
content-addressed input artifact，投影引用其 sha256，并要求在规划、目标更新和
完成审计前重新读取。UI preview 仍可截短，两者不能共用预算。

### 8.3 动态 recovery capsule

保留现有内容并增强：

- 当前 Plan revision/hash。
- running/blocked/pending/completed task 计数与关键项。
- 最新完成 artifact 和 verification evidence 引用。
- 当前 revision 的 ready frontier，而不是在 projector 内重新调度。
- 最近 RunTurn 结果、blocker fingerprint 和 pause reason。
- 已用 token/time/Turn/compaction 计数。

projector 只能读取和投影，不得拥有 ready-frontier 算法、DAG loop、重试、取消或
plan validator。ready frontier 必须由现有 TaskRuntime/kernel 的权威结果提供。

### 8.4 多锚点约束

EKO 不应期待一段 prompt 抵抗百次压缩。约束力来自：

```text
TaskRun.goal / artifact hash       # 原始目标
TaskRuntimeContextProjector        # 每个模型边界刷新
CanonicalContext                   # AGENTS/project/skills
events.jsonl + run-state.json      # 精确进度与恢复
TaskPlan revision + task evidence  # 可执行工作和验收
worktree/files/tests               # 外部事实
completion gate                    # 最终证明
```

这比“把更长的历史塞给模型”更稳定，也更容易测试。

## 9. TaskContinuationRuntime

### 9.1 归属与职责

新增在 `echo-agent-app-core` 应用层，建议路径：

```text
echo-agent-app-core/src/tasks/task_runtime/continuation.rs
```

它只负责：

- 处理 continuation request。
- per-run start-if-idle claim。
- 调用统一 `drive_chat`。
- 持久化 RunTurn outcome/usage。
- 根据 TaskRun 权威状态决定 stop、pause 或下一 Turn。
- boot resume 和用户控制。

它不负责：

- Plan authoring/patch。
- DAG validation 或 ready frontier。
- PlanTask 重试/取消/死锁判断。
- Subagent 调度或 worktree 集成。
- completion blocker 的第二套实现。

### 9.2 start-if-idle 协议

所有启动来源都调用同一个方法：

```text
request_continue(run_id, origin)
  -> acquire per-run continuation lock
  -> reload run-state from file authority
  -> require status == Running
  -> require continuation.enabled
  -> require !deferred && active_turn == None
  -> require budget available
  -> atomically append RunTurnStarted(ordinal, turn_id)
  -> register active_chat_turns / cancellation
  -> release persistent claim only after driver owns Turn
  -> drive_chat(... existing run_id ...)
```

如果两个 completion callback、用户 resume 和 boot recovery 同时触发，只有一个
能提交该 ordinal 的 `RunTurnStarted`。其余读取到 active turn 后返回
`NotSubmitted(AlreadyRunning)`，不得报错或再启动。

### 9.3 Turn 结束决策

```text
RunTurn ends
  -> idempotently account usage/time/compactions
  -> reload TaskRun + latest Plan revision
  -> if Completed/Failed/Cancelled: stop
  -> if Paused: stop and surface reason
  -> if cancellation is pause: transition Paused(reason)
  -> if terminal Agent error:
       classify retryable/unrecoverable
       update blocker/error audit
  -> if Running and continuation allowed:
       enqueue request_continue(run_id, Continuation)
```

continuation runtime 不运行第二套 completion gate，也不自行把 TaskRun 写成
`Completed`。`task_execute`、现有 executor 和 TaskRuntime store 仍独占任务完成
判断；runtime 只读取其终态。长程 Run 在没有调用 `task_execute`、或完成门禁未通过
时保持 `Running`，由下一 Turn 继续修复缺口。

续跑通过队列/异步任务重新进入 runtime，不在当前 Turn future 里递归调用，避免栈式
生命周期、取消 ownership 和资源 lease 纠缠。

### 9.4 deferral 与用户输入

当用户对 active TaskRun 输入新消息时，所有表面统一提供以下语义：

- **Steer**：当前 Turn 正在运行时，把补充上下文注入该 Turn，并持久化用户消息。
- **Continue/Resume**：没有目标变更，清除可恢复 pause 并启动下一 RunTurn。
- **Update Goal**：暂停、CAS 更新 TaskRun.goal，并要求 Plan revision 对齐。
- **Pause**：先持久化 Paused(User)，再取消 active Turn。
- **Cancel**：先持久化 Cancelled，再取消 active Turn/Subagent。
- **New task**：保留旧 Run，创建新 TaskRun，不复用 run_id。

在处理用户消息前设置 continuation deferral；消息已被 Turn 接管或用户操作完成后
再清除。这样用户输入不会与自动下一 Turn 竞速。

## 10. 完成、受阻与资源审计

### 10.1 completion authority

不新增 `goal_complete` 或 `task_complete` 工具。长程 Run 默认要求物化正式 Plan；
即使只有一个简单工作，也可建一个 PlanTask。完成权威继续是：

```text
TaskRun goal
+ exact TaskPlan revision
+ every required PlanTask terminal acceptance
+ required artifacts and execution checks
+ reviewer/integration outcome
+ run_completion_blockers == empty
```

主 Agent 可以通过现有 `task_create/task_update/task_execute` 推进任务，但不能仅凭
最终回答改变 TaskRun 为 Completed。`task_execute`/executor 和 store completion
gate 是唯一写入终态的路径。

若当前 `AllowDirect` 用于短小 unattended 任务，可保持普通一次性模式；显式长程
Run 使用 `RunPlanPolicy::RequirePlan`。这避免为 direct answer 再造一套完成证明。

### 10.2 最终 synthesis

现有 `task_execute`/executor 的 completion gate 通过后，store 立即将 TaskRun
转为 `Completed`，`RunCompleted` 事件补充确切 Plan revision、completion blocker
审计结果和证据引用。任务事实不依赖最终展示文案是否成功。

调用 `task_execute` 的当前 RunTurn 随后继续生成面向用户的 final synthesis；这与
普通工具调用后继续回答相同，不需要另开 Goal continuation。Goal contract 此时
告诉模型只基于已持久化证据总结，不再修改 PlanTask。

若进程或 provider 恰好在 `Completed` 已持久化、最终 assistant message 尚未落盘
时中断，TaskRun 保持 `Completed`。恢复层只生成一个 presentation-recovery Turn，
从 `RunCompleted` 证据和 TaskRuntime artifact 重建最终总结；它不改变 TaskRun
状态、不重跑 PlanTask/Subagent，也不计入 active Goal continuation。用以下两个
crash test 固定唯一语义：完成事件前中断仍按正常 TaskRuntime 恢复；完成事件后、
最终消息前中断只恢复展示。

### 10.3 blocker audit

每个无法推进的 Turn 产生规范化 `BlockerFingerprint`：

```text
category + stable resource/tool identity + normalized cause hash
```

规则：

- 能换路径继续工作时，不计为 blocker。
- 第一次/第二次相同 blocker 只记录，并尝试其它可行工作。
- 连续第三个 Goal RunTurn 仍为同一 blocker，且没有其它 ready work，转
  `Paused(RepeatedBlocker)`。
- 用户 resume 后清空连续计数，开始新 audit。
- indeterminate side effect 不等待三次，立即 `Paused(IndeterminateSideEffect)`，
  因为重复可能造成数据损坏。

### 10.4 预算

- token budget 只在用户明确设置时启用。
- 每个 provider usage event 按 id 幂等累计。
- 达到预算先完成本次记账，再原子转 `Paused(TokenBudget)`。
- usage limit、provider quota 与用户 token budget 分开记录。
- 预算接近耗尽不是降低完成标准的理由。
- UI/TUI 显示 used/budget/remaining；无预算时显示累计值，不伪造上限。

## 11. 进程恢复与副作用安全

### 11.1 boot 流程

扩展现有 `recover_incomplete`：

```text
scan Running TaskRun
  -> close orphan active RunTurn as Interrupted
  -> inspect task/subagent/tool boundaries
  -> reuse durable completed Subagent results
  -> Pending: replay-safe interrupted work
  -> Blocked: indeterminate mutating side effects
  -> transition Paused(BootRecovery)
  -> after AppState/services/surfaces ready:
       if continuation.enabled && auto_resume_after_restart
          && no indeterminate side effect
       then Running + request_continue(Recovery)
```

先 Paused 再恢复，保证用户启动应用后能看到一致 snapshot，也避免 TaskRuntime store
尚未注册到 projector 时就抢跑。

### 11.2 恢复身份

继续使用现有稳定 Subagent identity：

```text
<run_id>:<task_id>:<plan_revision>:<attempt>
```

RunTurn identity 不取代它。主 Agent RunTurn 重启可以换新 `turn_id`，但已完成的
PlanTask/Subagent attempt 必须由稳定 identity 复用。

### 11.3 transcript 与控制状态分离

- 用户/assistant 可见消息继续由 `FileConversationStore` 持久化。
- 自动 continuation context 只进入模型内部输入和 TaskRuntime event，不显示成
  用户消息。
- 完整工具/执行事实在现有 trace/artifact/TaskRuntime 文件中，不塞进
  `RunTurnSummary`。
- UI history 可投影“第 N 轮继续、压缩、暂停、恢复”等 milestone，但这些不是
  conversation role message。

## 12. 多界面功能对等

共同 service API 建议：

```text
start_long_horizon(run_id, options)
pause_run(run_id, reason=User)
resume_run(run_id)
cancel_run(run_id)
steer_run(run_id, prepared_user_turn)
update_run_goal(run_id, expected_revision, prepared_user_turn)
get_run_progress(run_id)
```

这是一组 EKO 应用 service 方法，不是新的模型任务 CRUD。

### 12.1 GUI

目标条显示：状态、当前 RunTurn、Plan 完成度、Subagent 活动数、累计 token/时间、
压缩次数和 pause reason。提供 pause/resume/cancel/edit 操作。复杂明细继续复用现有
Task panel，不新建另一套 Goal panel。

### 12.2 TUI

必须有与 GUI 等价的状态行/面板和命令；不能因 TUI 当前接线不完整而省略
TaskRuntime 或 continuation。事件源仍是 `ChatDriverEvent` 与 TaskRuntime snapshot。

### 12.3 CLI/channel

- 非交互 CLI 可用结构化 JSON 事件输出 RunTurnStarted/Finished、usage、pause 和
  terminal TaskRun。
- channel 使用同一 service；自然语言“继续/暂停/取消”映射到明确控制操作。
- 无法承载交互审批的 channel 将 run 置 Paused(Approval/NeedsInput)，不能静默
  失败或另走简化 executor。

### 12.4 控制权限

用户直接点击/输入的 pause、resume、terminal、MCP 和文件操作是交互行为，不受
Agent 自动执行 `permission_mode` 门控。自动 RunTurn 内的工具行为继续遵守现有
Agent permission policy。

## 13. 并发、取消与资源所有权

### 13.1 锁顺序

建议固定顺序，避免死锁：

```text
per-run continuation lock
  -> TaskRuntimeStore per-run transaction/append lock
  -> active_chat_turns registration
```

不得在持有同步 file-store lock 时 await model、sink、HITL 或 Subagent。

### 13.2 取消

- `Pause`：持久化 Paused(User) 后取消当前 Turn；PlanTask/Subagent 根据现有取消
  协议进入可恢复状态。
- `Cancel`：持久化 Cancelled 后传播到 Turn、DAG 和 Subagent。
- surface disconnect：不等于用户 cancel。前台可按产品策略 pause，显式 unattended
  Goal 继续后台运行。
- app shutdown：先 deferral/flush，再由 boot recovery 接管未结束 RunTurn。

### 13.3 backpressure

实时 token/thinking 仍是高频流；lifecycle、usage accounting、pause、completion
和 error 是不可丢终态。关键事件必须可靠 append/发送，不能用可能静默丢弃的
best-effort 路径。

## 14. 观测与诊断

每个日志/事件至少携带可用的 identity 子集：

```text
conversation_id
run_id
turn_id
turn_ordinal
plan_revision
task_id
subagent_run_id
event_id
```

需要的聚合指标：

- TaskRun token/time/Turn/compaction 总量。
- 每 Turn 时长、模型调用数、工具调用数和终止原因。
- continuation start requested/started/not-submitted 原因。
- boot recovery 数、重复执行拦截数、indeterminate side-effect 数。
- blocker fingerprint 连续次数。
- completion gate 每个 blocker 的直接证据引用。
- Goal contract projector 缺失/读取失败次数。

敏感工具参数、密钥和全文用户输入不得进入普通日志。objective 记录 hash 和 UTF-8
安全 preview；完整内容留在既有受控 artifact/TaskRuntime 文件。

## 15. 实施阶段

每个阶段都必须切换真实主路径并删除被替代逻辑；不允许只添加新抽象、让旧路径
长期并存。若一个提交只完成部分迁移，必须同步更新 `docs/MASTER-PLAN.md`，写清
当前权威路径和下一阶段删除目标。

### Phase 0：基线与契约测试

- 固化当前 TaskRun/TaskPlan/RunTurn identity 行为测试。
- 增加一个失败测试，证明当前 `finalize_task_mode_run` 会错误终结未完成长程 Run。
- 增加 projector 多次 prepare/多次 compact 的基线测试。
- 记录所有 GUI/TUI/CLI/channel `drive_chat` 调用点。

交付：只建立可重复证据，不新增并行 runtime。

### Phase 1：Run-bound Turn 与 ChatTurnOutcome

- 引入 `RunTurnBinding` 和 `ChatTurnRequest`。
- 分离 `run_id`、`turn_id`、`root_message_id`。
- `drive_chat` 返回 `ChatTurnOutcome`。
- 所有 surface 和 unattended 入口迁移到新签名。
- 删除从每个 continuation `turn_id` 派生新 formal run 的可能路径。

验收：普通 Chat 行为不变；一个现有 TaskRun 可连续执行两个不同 turn_id，工具和
projector 始终看到同一个 run_id。

### Phase 2：事件与 context projection

- 在 `RuntimeEventKind` 增加 continuation/RunTurn 事件。
- 在 `RunStateSnapshot` 增加 event-folded continuation 投影。
- 扩展 `TaskRuntimeContextProjector` 为 Goal contract + recovery capsule。
- 无 Plan 时也投影完整 Goal contract。
- 删除稳定 Goal 对 420 字符 recovery preview 的依赖。

验收：模拟 100 次 prepare/压缩后，每次模型输入均只包含一个最新 Goal contract
和一个最新 recovery capsule；原 objective hash 不变。

### Phase 3：TaskContinuationRuntime

- 实现 per-run start-if-idle、deferral、Turn ordinal claim。
- 用 `finalize_run_turn` 替换 `finalize_task_mode_run`。
- Running long-horizon Run 在 Turn EOF 后自动启动下一 Turn。
- 普通一次性 Task mode 保持显式旧语义，直到产品统一决定是否默认连续执行。
- 将 `drive_agent_run` 的平行 stream-finalization 归一到共同 outcome。

验收：并发触发 100 次 continue 只启动一个 Turn；Turn 完成回调与用户 resume
竞态不重复启动。

### Phase 4：预算、暂停、恢复和用户控制

- 实现幂等 usage accounting 和 token/time budget。
- 实现 pause reason、blocker audit、user deferral。
- 扩展 boot recovery 与可选 auto-resume。
- 打通 steer/update/pause/resume/cancel service。
- 对 indeterminate mutating side effect 保持人工恢复。

验收：在 tool 调用前后、Subagent 完成前后、event append 前后分别 kill 进程，
恢复后无重复完成、无错误自动重放、usage 不翻倍。

### Phase 5：多界面功能对等

- GUI 复用现有 Task panel 增加 Goal/RunTurn 控制投影。
- TUI、CLI、channel 接入相同 service 和事件。
- 生成/更新 TypeScript 契约。
- 删除任何 surface 私有的 resume/cancel 推断逻辑。

验收：同一个 TaskRun 可从 GUI 启动、TUI 暂停、CLI 恢复、channel 查询，所有
surface 观察到同一状态和计数。

### Phase 6：完成审计与长程评测

- 长程 Run 默认 `RequirePlan`。
- 将现有 `run_completion_blockers` 作为唯一完成门禁。
- 增加 final synthesis 的唯一选定语义和 crash recovery。
- 建立真实长任务 eval、drift score 和 soak test。
- 删除实现过程中暴露的旧单 Turn Task 旁路和过时文档。

验收：完成声明必须能逐项链接到 Goal requirement、PlanTask、artifact 和测试证据；
缺一项时保持 Running/Paused，不输出伪 Completed。

## 16. 测试与验证矩阵

### 16.1 单元测试

- RunContinuationState 的事件 fold/replay/判重。
- UTF-8 objective、emoji、长 artifact reference 和 preview。
- token/time checked/saturating accounting。
- pause reason 与 TaskRunStatus 合法组合。
- blocker fingerprint 连续/重置规则。
- RunTurn ordinal CAS 与 start-if-idle。
- projector 无 Plan、有 Plan、revision 更新、store error。

### 16.2 集成测试

- 一个 Goal 跨 3 个 Turn、每 Turn 多次 compact。
- 用户在自动 continuation 提交前发送 steer。
- pause 与 Turn completion 同时发生。
- Goal update 导致旧 Plan revision 暂停执行。
- budget 在 usage event 边界耗尽。
- TaskRun 完成后陈旧 continuation callback 到达。
- boot recovery 复用 completed Subagent result。
- mutating tool 状态不确定时禁止自动 replay。
- conversation reconnect 不生成重复可见用户消息。

### 16.3 property / stress 测试

- 任意事件重放次数下 usage 不增长。
- 任意 continuation 请求并发顺序下 active Turn 数不超过 1。
- 任意 compact 次数下 Goal contract 唯一且 objective hash 不变。
- 任意 Plan revision safe point 下已完成 task 不重跑。
- 至少 100 RunTurn、100 次压缩的 fake-model soak。

### 16.4 真实场景 eval

至少覆盖：

1. 多仓库、数百 finding、数万行潜在改动的修复任务。
2. 大型 feature，从调研、Plan、实现到全门禁。
3. 中途两次进程重启和一次机器休眠。
4. 用户中途修改一项 requirement。
5. 一个暂时 provider failure 和一个真实需要用户输入的 blocker。
6. GUI/TUI/CLI/channel 交叉控制同一 Run。

评测不仅看最终 tests green，还要比较：原始 requirement 覆盖、无范围缩小、无重复
副作用、Plan revision 一致性、所有 terminal 状态诚实、用量精确和恢复时间线。

### 16.5 仓库门禁

实现触及 Rust、feature、公共 API 或前端时，按仓库 `AGENTS.md` 执行全部适用
fmt、clippy、test、no-default、GUI 和前端矩阵。任何失败都必须修复。CLI 侧静态
审计必须保持无 SQLite、无绝对 worktree Cargo 路径、无平行任务 CRUD。

## 17. 风险与缓解

| 风险 | 后果 | 缓解 |
|---|---|---|
| Goal 与 TaskRun 双权威 | 状态漂移、无法判定完成 | 不新增 Goal store/type，TaskRun 唯一权威 |
| Turn EOF 被当成完成/失败 | 长任务提前终止 | `ChatTurnOutcome` + `finalize_run_turn` |
| 自动续跑竞态 | 重复模型调用和副作用 | per-run lock + event CAS + start-if-idle |
| usage 重复记账 | 预算提前耗尽 | 稳定 event_id + store 原子判重 |
| 压缩后 Goal 漂移 | 做成较小/不同任务 | 每模型边界 stable Goal projection + artifact hash |
| projector 变成第二调度器 | 两套 ready frontier | projector 只读权威 projection |
| crash 重放写副作用 | 数据损坏 | 现有 tool/Subagent boundary + indeterminate pause |
| 状态枚举膨胀 | 迁移和逻辑组合爆炸 | 6 状态不变，细节放 pause reason/event |
| 自动消息污染 transcript | 用户看到伪输入 | internal continuation visibility |
| surface 行为分叉 | TUI/CLI 功能缺失 | 统一 service、drive_chat、event/snapshot |
| completion 依赖最终话术 | 伪完成 | TaskRuntime evidence gate 唯一终态写入 |
| 长运行磁盘无限增长 | 性能下降 | append-only authority + 可验证 snapshot/retention，禁止删恢复所需事件 |

## 18. 建议的首个实现切片

最小但真正切换主路径的首个切片应同时完成：

1. `RunTurnBinding`，允许已有 TaskRun 使用新 turn_id 调 `drive_chat`。
2. `ChatTurnOutcome`，至少返回 usage、compaction count、elapsed 和 terminal。
3. `RunTurnStarted/Finished/UsageAccounted` 事件及 run-state fold。
4. Goal contract 在无 Plan 时也由现有 projector 每模型边界投影。
5. 将长程 TaskRun 的 `finalize_task_mode_run` 行为替换为“结束 Turn、保持 Run
   Running”，再由一个 start-if-idle continuation 启动第二 Turn。

这个切片能用一个“两 Turn + 多次 compact”的集成测试证明核心闭环，且没有引入
第二套 Plan、store 或 executor。之后再加 boot auto-resume、预算和 UI 控制。

## 19. 最终验收标准

实现完成必须同时满足：

- 一个 TaskRun 能跨任意多个有限 RunTurn 自动推进。
- 一个 RunTurn 能经历多次压缩，Goal contract 在每个模型边界可验证存在。
- app restart 后恢复同一 TaskRun，已完成 PlanTask/Subagent 不重复执行。
- 同一 run 永不并发启动两个主 Agent Turn。
- Goal、Plan、执行、Todo 和 UI 不出现第二权威。
- token/time 记账在重试、重放和 crash 下幂等。
- pause/resume/cancel/update 在 GUI/TUI/CLI/channel 功能对等。
- 用户交互功能不受 Agent 自动权限模式错误门控。
- completion 由原始 Goal + TaskRuntime 证据证明，不由 stream EOF 或最终话术推断。
- `echo-agent-cli` 不启用 SQLite，产品术语只使用 Subagent。
- 所有适用提交门禁、长程 stress/eval 和静态审计全部通过。

## 20. 参考资料

- [OpenAI Codex Goal runtime](https://github.com/openai/codex/blob/53eaa297e595fc98df0f33d4c63686a7014d7c9a/codex-rs/ext/goal/src/runtime.rs)
- [OpenAI Codex Goal steering](https://github.com/openai/codex/blob/53eaa297e595fc98df0f33d4c63686a7014d7c9a/codex-rs/ext/goal/src/steering.rs)
- [OpenAI Codex compaction](https://github.com/openai/codex/blob/53eaa297e595fc98df0f33d4c63686a7014d7c9a/codex-rs/core/src/compact.rs)
- [Cursor: Expanding our long-running agents research preview](https://cursor.com/blog/long-running-agents)
- [Cursor: What we have learned building cloud agents](https://cursor.com/blog/cloud-agent-lessons)
- [Claude Code 官方 changelog](https://github.com/anthropics/claude-code/blob/be90077c6a353f292fa612d97173865a9ab21b83/CHANGELOG.md)
- [LangGraph persistence](https://github.com/langchain-ai/docs/blob/c26a7ab8aea6c871b0c9c9f79e0a2544d57c7d1d/src/oss/langgraph/persistence.mdx)
- [EKO Dynamic Plan Runtime](./2026-07-21-dynamic-plan-runtime.md)
- [EKO Runtime DAG Kernel Convergence](./2026-07-27-runtime-dag-kernel-convergence.md)
- [EKO Framework / App Boundary](./framework-app-boundary-plan.md)
- [EKO Master Plan](./MASTER-PLAN.md)
