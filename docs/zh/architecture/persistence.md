# EKO 的 Store、Journal、Checkpoint 与 Trace

本文说明 EKO 如何组合 `echo-agent` 提供的通用 Store、Journal、Checkpoint 和 Trace 原语。通用 trait 与实现语义以 `echo-agent` 文档为准；本文只定义 EKO 的产品数据作用域和权威关系。

## EKO 中的概念边界

| 概念       | EKO 实例                                                                                      | 产品职责                           |
| ---------- | --------------------------------------------------------------------------------------------- | ---------------------------------- |
| Store      | `FileStore`、`FileConversationStore`、`FileRuntimeStateStore`、`RunStore`、`TaskRuntimeStore` | 为不同领域提供读写边界             |
| Journal    | TaskRuntime `events.jsonl`、普通聊天 `ChatEventLog`                                           | 保存有序产品事实和可靠交付流       |
| Checkpoint | TaskRuntime `checkpoint.json`、framework `AgentCheckpoint`                                    | 加速任务投影恢复或继续 ReAct 会话  |
| Trace      | workspace `traces/` 下的 `Run`/`RunEvent`                                                     | usage、cache、tool、错误和耗时诊断 |

`Store` 是重载名称，不表示这些类型共享同一数据模型。讨论状态时必须带领域限定，例如“memory Store”“conversation Store”“TaskRuntime journal”或“trace RunStore”。

## Turn 与正式任务:TaskRuntime Journal 是事实权威

`TaskRuntimeStore` 是 EKO turn-run 与正式任务的应用门面。每个 store-backed turn 都急切
绑定 TaskRun;`TaskRunExecutionProfile` 区分内部 conversation-turn journal 与应投影到任务
界面的 orchestrated run。实际事件权威由 `RunAuthority` 管理，它是对 framework
`FileEventJournal + FileCheckpointStore + CheckpointedReducer` 的薄适配。

```text
TaskRuntime mutation
        │
        ▼
RunAuthority
        │
        ├─> events.jsonl       事实历史
        ├─> checkpoint.json   可重建 checkpoint
        ├─> artifact-history.jsonl          可重建 Artifact read model
        ├─> artifact-history.meta.jsonl     append-batch count/sequence/hash-chain frames
        ├─> review-history/<safe-key>.jsonl 可重建 per-task Review read model
        ├─> review-history/<safe-key>.meta.jsonl per-task append-batch 完整性 frames
        ├─> history-cursor.json             read-model source cursor
        ├─> plan.json         确定性读投影
        └─> run-state.json    确定性读投影
```

关键不变量：

1. 每次 turn、正式任务与 user-steer 状态变化先提交到 `events.jsonl`。
2. sequence、batch identity、重放、crash-tail repair 和 checkpoint recovery 使用 framework Journal 原语。
3. `EventFoldState` 从事件生成任务、plan、todo、usage、continuation 和恢复相关投影。
4. `checkpoint.json` 保存已 fold sequence 和 `EventFoldState`，损坏或落后时从 Journal 重建。
5. `plan.json`、`run-state.json` 是 read projection，不建立第二套状态机或 mutation owner。
6. Todo、latest Summary 和 Completion Gate 与运行态共享有界的
   `EventFoldState/checkpoint.json`。Artifact/Review 全历史不进入每事件重写的 checkpoint；它们
   分别进入增量 Artifact segment 和按 stable task key 安全编码的 Review segment。生产查询只
   扫描返回结果对应 segment 加未投影 suffix，不从 sequence 0 重扫 `events.jsonl`。
7. history segment 与 cursor 都没有独立 mutation/sequence authority；共享 cursor 只在所有相关
   segment 及其 companion metadata durable 后推进。metadata 保存 count、最后 relevant source
   sequence 和增量 SHA-256 hash-chain，查询全量扫描返回结果时重算校验，能识别语法合法的前缀
   截断或空文件。segment 缺失、损坏或 partial crash 时，`RunAuthority` 从 Journal 去重重放或
   重建；完整重建会替换 review 目录，清除 stale segment。
8. Trace 不能用于判断 PlanTask 或 TaskRun 是否已经提交。

### 为什么 checkpoint 不是第二份权威

恢复过程先验证 checkpoint 是否不超过 Journal 尾部，再加载 checkpoint 并重放缺失后缀。checkpoint 缺失、损坏或过期时，`CheckpointedReducer` 从 `events.jsonl` 重建并修复它。

TaskRuntime checkpoint 还带 EKO query projection schema。旧 schema 即使 sequence 位于 journal
head 也不能当作新鲜投影，因为新增索引会缺失；运行时会在 recovery 中逻辑忽略旧 frame、
重放完整 journal，再 best-effort 原子发布新 checkpoint。删除或写入因只读权限失败时只记录
degraded observability，不阻断正确查询；该迁移不改变事件格式，也不会产生第二份事实权威。

因此：

```text
可以删除 checkpoint 后从 events.jsonl 恢复；
不能删除 events.jsonl 后把 checkpoint 当成完整事实历史。
artifact/review history segment 和 history cursor 同样可以删除重建；它们不能替代 events.jsonl。
```

## 普通聊天：独立的 Chat Journal

普通 Chat 同时使用 `ChatEventLog` 保存 GUI、TUI、CLI、channel 和 boot recovery 消费的
有序交付事件流。它也拥有一个 conversation provenance 的 TaskRun,用于 Goal、steer、
continuation 与恢复;无 plan conversation run 不进入任务 UI。

职责分层如下：

| 层                                    | 职责                                                                                                    |
| ------------------------------------- | ------------------------------------------------------------------------------------------------------- |
| framework `SegmentedFileEventJournal` | physical sequence、segment、integrity、durability、recovery、pruning                                    |
| EKO `ChatEventLog`                    | workspace/conversation/root-turn identity、retention pin、queued input/command_cell_watch fact、surface projection |

Journal append 是 surface 交付边界。surface 接收 journaled event，而不是各自建立不可重放的后端事实源。

`ChatEventLog` 与 TaskRuntime `events.jsonl` 的事实不同:前者负责输入/输出交付与 surface
重放,后者负责 turn-run Goal、约束和执行状态。二者可以关联同一 turn,但不能合并为进程
全局 Journal,也不能互相替代。完整决策见 [ADR 0037](../adr/0037-unified-turn-run-binding.md)。

## ReAct Checkpoint 与 ConversationStore

Workspace runtime 同时配置：

- `FileRuntimeStateStore`：保存 framework `AgentCheckpoint`，恢复完整 ReAct 消息、plan 文本、激活技能、blocked reason 和 working directory；
- `FileConversationStore`：保存用户可见 transcript projection，供会话列表和历史界面读取。

二者可能使用相同的 `conversation_id`，但不能互相替代：

| 对比                     | Runtime checkpoint         | Conversation transcript |
| ------------------------ | -------------------------- | ----------------------- |
| 目标                     | 继续未完成的 Agent runtime | 展示用户可见历史        |
| 是否含内部 tool hand-off | 是                         | 只保存投影后的可见消息  |
| 主要消费者               | Agent hydration            | GUI/TUI/CLI history     |
| 是否拥有 Task DAG        | 否                         | 否                      |

正式任务的 Task DAG 和生命周期仍由 TaskRuntime Journal 拥有，不能塞进 `AgentCheckpoint`。

## Trace：诊断，不是恢复权威

EKO 为 Agent 配置 `RunStore` 后，会保存 invocation Trace，并通过 `/trace`、`/runs` 和 observability panel 展示：

- provider usage 和缺失 usage；
- prompt/cache/context breakdown；
- LLM 和 tool 调用；
- context compression；
- Subagent invocation；
- status、error 和 timing。

Trace 写入失败当前不会回滚 TaskRuntime 已提交事件，因此 Trace 只能作为诊断事实，不能成为 TaskRun、PlanTask、tool completion 或 checkpoint recovery 的权威。

## 当前权威矩阵

| 数据                | 权威/主要边界                               | 可否重建                                         | 不负责什么             |
| ------------------- | ------------------------------------------- | ------------------------------------------------ | ---------------------- |
| 长期记忆            | workspace `FileStore`                       | 取决于上层证据来源                               | 不恢复 ReAct 循环      |
| 用户可见历史        | `FileConversationStore`                     | 可从仍保留的产品事件重新投影，但不能假定永远完整 | 不保存内部完整上下文   |
| ReAct 恢复状态      | `FileRuntimeStateStore` / `AgentCheckpoint` | 不能假设可由 Task journal 重建                   | 不拥有 Task DAG        |
| 正式任务事实        | TaskRuntime `events.jsonl`                  | 它本身是重建来源                                 | 不承担 usage 诊断      |
| 正式任务 checkpoint | `checkpoint.json`                           | 可从 `events.jsonl` 重建                         | 不是事实权威           |
| 普通聊天产品流      | `ChatEventLog`                              | 按 retention 范围重放                            | 不替代正式任务 Journal |
| 执行观测            | Trace `RunStore`                            | 不是产品状态重建来源                             | 不决定业务提交         |

## 文件位置

```text
<workspace>/.eko/
  conversations/       FileConversationStore
  memory/store.json    FileStore
  sessions/            FileRuntimeStateStore / session state
  tasks/<run-id>/
    events.jsonl       TaskRuntime fact journal
    checkpoint.json    derived fold checkpoint
    plan.json          read projection
    run-state.json     read projection
  traces/              RunStore execution traces
```

具体目录必须通过 `WorkspaceLayout` 或 framework path API 解析，不得复制硬编码路径。

## 代码入口

- Workspace runtime 绑定：`echo-agent-app-core/src/infra/factory.rs`
- TaskRuntime facade：`echo-agent-app-core/src/tasks/task_runtime/store/mod.rs`
- Task Journal/checkpoint authority：`echo-agent-app-core/src/tasks/task_runtime/run_authority.rs`
- Task projection/read side：`echo-agent-app-core/src/tasks/task_runtime/file_store.rs`
- 有界查询投影决策：`docs/adr/0008-taskruntime-bounded-query-projections.md`
- Task recovery/shadow path：`echo-agent-app-core/src/tasks/task_runtime/file_shadow.rs`
- 普通 Chat Journal：`echo-agent-app-core/src/chat_event_log/journal.rs`
- Observability projection：`echo-agent-app-core/src/observability/`
- Workspace 文件布局：`echo-agent-app-core/src/workspace/layout.rs`
