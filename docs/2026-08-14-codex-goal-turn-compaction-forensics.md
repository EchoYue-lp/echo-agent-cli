# Codex Goal、Turn 与压缩长程运行机制取证

> 日期：2026-08-14
> 结论性质：本机实例取证 + OpenAI Codex 官方公开源码交叉验证
> 本机 Codex CLI：`codex-cli 0.144.1`
> 官方源码快照：`openai/codex@53eaa297e595fc98df0f33d4c63686a7014d7c9a`

## 1. 结论

此前关于 Codex 长程任务的核心解释是保真的：`Goal` 不是一条很长的
prompt，也不是一个无限延长的 Turn，而是挂在 Thread 上、独立持久化、能够
跨 Turn 自动续跑的任务控制面。Turn 是一次有明确开始和结束的执行尝试；一次
Turn 内部又可以因上下文压缩产生多个 context window。Goal、Turn、window
处于不同生命周期层级。

但是，若把此前口头解释保存为工程依据，必须做两项精确修正：

1. 该实例的 rollout 一共包含 6 个 Turn；其中第 1 个发生在 Goal 创建前，
   第 6 个发生在 Goal 完成后。Goal 实际覆盖 4 个 Turn，而不是 6 个。
2. Goal 会在每次**自动续跑 Turn**启动前从持久化状态重新读取并注入完整目标；
   不能声称 Goal 扩展会在同一 Turn 的每一次压缩后重新查询 Goal 数据库。
   同一 Turn 内的约束保持由压缩重建、上下文投影、历史摘要和外部权威状态共同
   完成。

除此之外，截图中的核心数字均能由本机 rollout 复核：

- Goal 创建于 2026-08-13 01:07:43 UTC，完成于 20:17:38 UTC。
- Goal 累计记账 `7,541,290` tokens。
- Goal 累计有效运行时间 `50,611` 秒，即 14 小时 3 分 31 秒。
- Goal 覆盖 4 个 Turn。
- 这 4 个 Goal Turn 合计发生 22 次自动上下文压缩。
- 最长的单 Turn 从 01:06:00 持续到 12:49:25，约 11 小时 43 分，期间
  发生 10 次压缩。

因此，更准确的表述是：**Codex 用一个持久 Goal 编排多个有限 Turn，每个 Turn
又可以跨越多个压缩窗口；长程可靠性来自持久目标、自动续跑、反复上下文重建和
外部证据四者的冗余约束，而不是来自模型一次记住几百万 token。**

## 2. 证据等级与边界

本文将证据分成三层，避免把观察、源码事实和推断混在一起：

| 等级 | 含义 | 本文使用方式 |
|---|---|---|
| A | OpenAI 官方公开源码或本机协议/schema | 可描述具体数据结构和控制流 |
| B | 本机真实 rollout、Goal 状态库和界面结果 | 可描述该实例的时间线与计数 |
| C | 由 A/B 推导出的架构解释 | 明确标注为工程推论，不冒充官方承诺 |

本机 rollout 路径为：

```text
~/.codex/archived_sessions/
rollout-2026-08-13T08-56-02-019ff89e-74e7-7aa1-a4b5-4184c85e2ae9.jsonl
```

该文件包含用户内容、工具参数和本机路径，不应提交进仓库。本文只记录计数、
时间边界和机制性事实，不复制完整私有 transcript。

OpenAI 公开源码是持续演进的实现，本文固定到上述 commit。源码与本机二进制
并非同一次构建，但核心 Goal schema、状态、工具和续跑控制流可以相互印证。
它们不是对未来版本的稳定 API 保证。

## 3. 四层运行模型

### 3.1 Thread：交互与恢复容器

Thread 是一组连续交互的容器，拥有 transcript、rollout、当前模型上下文、
Goal 关联和恢复身份。用户在 Codex UI 里看到的一个任务/会话通常对应一个
Thread。

Thread 本身并不等于 Goal。没有 Goal 的普通对话也有 Thread；Goal 完成后，
同一 Thread 还可以继续发生新 Turn。

### 3.2 Goal：跨 Turn 的持久任务控制面

Goal 挂在 `thread_id` 上，至少持久化以下信息：

```text
thread_id
goal_id
objective
status
token_budget
tokens_used
time_used_seconds
created_at_ms
updated_at_ms
```

本机 `~/.codex/goals_1.sqlite` 的 `thread_goals` 表还对状态做了约束：

```text
active
paused
blocked
usage_limited
budget_limited
complete
```

另有一个按 `thread_id` 存储的 continuation deferral 记录，用于阻止自动续跑
与待处理输入或外部控制发生竞态。

Goal 的关键特性是：

- 生命周期长于一个 Turn。
- 原始 `objective` 独立于压缩后的聊天摘要。
- token 和时间按 Goal 聚合，而不是只按单 Turn 展示。
- Goal 状态为 `active` 且 Thread 空闲时，运行时可以自动创建下一 Turn。
- 暂停、受阻、用量限制、预算限制和完成都会停止自动续跑。

### 3.3 Turn：一次有限的执行尝试

Turn 是一次模型/工具循环的执行单元，有自己的 `turn_id`、输入、状态、事件、
取消边界和终止结果。一次用户消息通常启动一个 Turn；Goal runtime 也可以用
内部 continuation context 启动一个 Turn。

Turn 结束不等于 Goal 结束。对于活跃 Goal，Turn 可以有三种典型结局：

- 本轮做完一部分，Goal 仍是 `active`，运行时继续启动下一 Turn。
- 本轮证明全部目标完成，调用 Goal 更新工具标记 `complete`。
- 本轮遇到终止条件，Goal 进入暂停、受阻或资源限制状态。

### 3.4 Context window：Turn 内的模型输入窗口

一个 Turn 可以进行大量模型 round trip 和工具调用。当上下文接近模型限制时，
Codex 会压缩历史、生成摘要、推进 window identity，并在**同一 Turn**中继续。

因此四层关系是：

```text
Thread
└── Goal (optional, persistent)
    ├── Turn 1
    │   ├── context window 1
    │   ├── context window 2
    │   └── ...
    ├── Turn 2
    │   └── ...
    └── Turn N
```

Subagent 是 Turn/任务执行过程中使用的并行能力，不应与 Goal、Turn 或 window
混为同一个层级。

## 4. Goal 与 Turn 的区别和联系

| 维度 | Goal | Turn |
|---|---|---|
| 语义 | 用户希望最终达成的完整结果 | 为 Goal 或普通对话执行的一次尝试 |
| 归属 | Thread 级 | Thread 内的单次执行级 |
| 生命周期 | 可跨多个 Turn、进程恢复和长时间运行 | 有明确开始与终止 |
| 持久化 | 独立 Goal 状态与累计记账 | rollout/transcript 中的输入、事件和结果 |
| 上下文压缩 | objective 不依赖聊天摘要存活 | 同一 Turn 可跨多次压缩 |
| 调度 | active + idle 时可触发下一 Turn | 被用户或 Goal runtime 启动 |
| 完成判断 | 需要显式完成审计并更新 Goal 状态 | 流结束只表示本轮结束 |
| 预算 | 跨 Turn 聚合 token/时间 | 提供本轮增量供 Goal 记账 |
| 取消/暂停 | 决定是否还会续跑 | 终止当前执行 |

二者通过以下字段和事件关联：

- Goal runtime 记录当前活跃 Goal 对应的 Turn。
- Turn 开始时建立 Goal 记账基线。
- Turn 中的 usage 被计算为增量并幂等写入 Goal。
- Turn 完成后，如果 Goal 仍 active，runtime 尝试 `start_turn_if_idle`。
- Goal objective 被更新时，运行时可向正在执行的 Turn 注入 steering context。

## 5. 官方源码中的 Goal 实现

### 5.1 工具契约

官方 Goal 扩展暴露三个模型工具：

- `get_goal`：读取当前目标、状态、预算、token 和耗时。
- `create_goal`：仅在用户或更高优先级指令明确要求时创建；不能把普通任务擅自
  升级为 Goal。
- `update_goal`：模型只能将 Goal 标记为 `complete` 或严格意义上的
  `blocked`；暂停、恢复和资源限制由用户/系统控制。

工具描述还要求：相同阻塞条件连续出现在至少三个 Goal Turn 中，且确实无法
继续推进时，才能标记 `blocked`。这避免模型遇到第一次困难就提前终止长任务。

### 5.2 自动续跑控制流

`GoalRuntimeHandle::continue_if_idle` 的公开源码控制流可以概括为：

```text
确认 Goal 功能和工具可用
  -> 获取单许可 Goal 状态锁
  -> 检查 continuation deferral
  -> 获取当前 live Thread
  -> 从持久化状态重新读取 Goal
  -> 仅接受 status == active
  -> 构造内部 continuation context
  -> thread.start_turn_if_idle(...)
```

这里有四个重要工程点：

1. 状态锁覆盖“读取 Goal 到启动 Turn”的窗口，避免外部 pause/clear 在中间穿插。
2. 每次自动续跑都重新读取持久化 Goal，而不是复用可能过期的内存对象。
3. `start_turn_if_idle` 将“只能有一个活跃 Turn”的约束放在 Thread runtime，
   避免重复续跑。
4. deferral 独立于 Goal status，使“暂时让路给用户输入”不需要污染生命周期状态。

### 5.3 续跑上下文

续跑输入不是伪造的普通用户消息，而是带有内部来源 `goal` 的 contextual user
fragment。其内容由持久 Goal 重新构造，包含：

- 原始 objective。
- 已用 token、可选预算和剩余预算。
- 不得缩小目标的连续执行约束。
- 以当前 worktree/外部状态为权威的证据约束。
- 完成前逐项核验原始要求的 completion audit。
- 连续受阻审计规则。

这段上下文的核心价值不在于篇幅，而在于它每次都从结构化 Goal 数据生成，
不会把上一次摘要里的“我大概在做什么”误当成原始目标。

### 5.4 恢复与记账

Thread 恢复时，如果持久 Goal 仍为 active，Goal runtime 会恢复其 idle-active
记账状态。运行中的 Turn 和空闲等待阶段分别计算 token/time 增量，并通过
expected goal identity 防止旧 Turn 把用量写到后来替换的新 Goal 上。

官方源码还用串行许可保护进度记账，并为事件构造稳定 identity。这说明
`7,541,290` 不是最终界面从 transcript 粗略估算出来的数字，而是 Goal 控制面
累计的用量字段。

## 6. 压缩是如何工作的

### 6.1 压缩不会让 Turn 自动结束

Codex core 在压缩时克隆当前历史，执行压缩模型调用，获取 summary，然后用
replacement history 替换活跃上下文。压缩完成后推进 window identity、重新计算
token 用量，并继续原 Turn。

replacement history 至少由以下类别共同构成：

- 压缩摘要。
- 保留/整理后的用户消息。
- 重新构造的 initial context。
- 当前 Turn 的 reference context 和 world-state baseline。

这使系统/开发者指令、项目规则和当前环境状态不必完全依赖自然语言摘要存活。

### 6.2 不能把压缩描述成无损

官方 `compact.rs` 在压缩完成后会明确发出警告：长 Thread 和多次压缩可能降低
准确性，应尽量保持任务聚焦。这个警告非常重要：Goal 机制提高的是长程约束的
鲁棒性，不是数学意义上的无损记忆。

同一 Turn 内，公开 Goal 扩展没有显示“每次 compact 都重新查询 Goal store”
这一动作。因此本文不作这种超出证据的承诺。能够确认的是：

- 第一次 Goal 创建发生在首个 Goal Turn 的第一次压缩之前。
- Goal 工具调用和 objective 已进入该 Turn 的真实历史。
- core compaction 会重建 initial context、用户信息和摘要。
- 下一次自动续跑 Turn 一定从持久化 Goal 重新生成完整 continuation context。

## 7. 为什么经过 22 次压缩仍能保持约束

不是某一个机制单独做到的，而是六个锚点叠加：

### 7.1 原始目标的独立持久化

`objective` 不以聊天摘要为权威。即使摘要遗漏细节，下一次自动续跑仍可从 Goal
状态重新获得完整目标。

### 7.2 每个自动续跑 Turn 的目标再注入

自动 Turn 的第一条内部上下文重新声明目标、预算、证据原则、完成审计和受阻
规则。跨 Turn 漂移不会无限累积。

### 7.3 每次模型边界的高优先级上下文重建

压缩不是只保留一段自由文本摘要。system/developer context、项目规则、当前
world state 和 reference context 会参与重建，形成多层约束。

### 7.4 工作区和结构化 artifact 是外部记忆

大任务的真实进度存放在代码、测试结果、计划、finding ledger、Git diff 和生成
文档中。模型每轮通过工具重新读取当前状态，而不是凭内部回忆决定“做过什么”。

### 7.5 显式 completion audit

Goal prompt 要求在完成前重新从原目标推导要求，并为每项要求找到当前权威证据。
这能抵抗一种常见长任务退化：模型把“做了很多”误判成“已经完成”。

### 7.6 显式工具状态，而不是靠最终话术

Goal 只有在模型调用 `update_goal(status=complete)` 后才结束。输出一段看似完整的
最终总结，不会自动把 Goal 变成完成态。

由此可得到一个可迁移的设计原则：

> 长程 Agent 的可靠性来自“短期上下文 + 持久目标 + 可恢复状态 + 外部 artifact
> + 严格完成证据”的组合，而不是来自扩大单次上下文窗口。

## 8. 本实例的精确时间线

| 序号 | UTC 时间 | 与 Goal 的关系 | 压缩次数 | 结果 |
|---|---|---:|---:|---|
| Turn 1 | 00:56:36 - 01:00:39 | Goal 创建前 | 0 | 只读复查并给出初步结论 |
| Turn 2 | 01:06:00 - 12:49:25 | Goal 在 01:07:43 创建 | 10 | 大规模修复与第一轮门禁 |
| Turn 3 | 12:50:27 - 16:36:13 | 用户继续同一 Goal | 5 | 补充完成审计与进一步修复 |
| Turn 4 | 16:36:13 - 18:57:15 | 自动 Goal continuation | 4 | 逐 finding 闭环 |
| Turn 5 | 18:57:15 - 20:17:48 | 自动 Goal continuation | 3 | 最终验证并标记 Goal complete |
| Turn 6 | 21:24:46 - 21:25:24 | Goal 完成后 | 0 | 用户要求提交两个仓库 |

22 次 `context_compacted` 的时间分布与上表一致。rollout 中同时有 22 个对应
compaction 持久记录。Goal 的 `create_goal` 工具调用发生于 01:07:43；
12:55:03 的 `thread_goal_updated` 是后续通知，不能误当作 Goal 创建时刻。

完成工具返回：

```text
status = complete
tokensUsed = 7541290
timeUsedSeconds = 50611
```

## 9. 常见误解

### 9.1 “14 小时就是一个 Turn”

错误。14 小时是 Goal 的累计有效时间；Goal 横跨 4 个 Turn。只是其中第一个
Goal Turn 本身也异常长，持续约 11 小时 43 分。

### 9.2 “7.5M token 都同时放进模型上下文”

错误。该数字是跨模型调用、跨窗口、跨 Turn 的累计 usage。模型任何时刻只看到
当前上下文窗口。

### 9.3 “压缩摘要本身保存了全部约束”

错误。摘要会丢信息。可靠性来自 Goal 独立持久化、上下文重建、外部 artifact 和
完成审计共同兜底。

### 9.4 “Turn 结束就是任务完成”

错误。Turn 是执行尝试，Goal 是最终任务。活跃 Goal 在 Turn 结束后可以自动
启动下一 Turn。

### 9.5 “Goal 是 Plan 的另一个名字”

错误。Goal 描述最终要达到什么；Plan 描述当前打算怎样达到。Plan 可以修订、
替换或部分失效，而 Goal 应保持稳定，除非用户明确修改目标。

## 10. 对 EKO 的直接启示

Codex 的 `ThreadGoal` 不能逐字段照搬到 EKO，因为 EKO 已有 `TaskRun`，且
`TaskRun` 的现有定义就是“一次用户 Goal”。直接复制会产生两个目标权威。

应迁移的是机制，而不是类型名称：

- 用 EKO `TaskRun.goal` 承担持久 objective。
- 用多个 run-bound Turn 承担分段执行。
- 用现有 `TaskRuntimeStore` 和 `events.jsonl` 承担恢复权威。
- 用现有 `TaskRuntimeContextProjector` 在每个模型边界重新投影 Goal contract。
- 用一个薄的 continuation runtime 在 run active 且 idle 时启动下一 Turn。
- 用现有 revisioned `TaskPlan`、DAG executor 和 Subagent runtime 执行实际任务。
- 用 TaskRuntime completion gate，而不是聊天流结束，判定整个 TaskRun 完成。

完整 EKO 方案见
[`2026-08-14-eko-long-horizon-task-runtime-design.md`](./2026-08-14-eko-long-horizon-task-runtime-design.md)。

## 11. 参考资料

OpenAI Codex 官方公开源码，固定到
[`53eaa297`](https://github.com/openai/codex/tree/53eaa297e595fc98df0f33d4c63686a7014d7c9a)：

- [Goal runtime 与自动续跑](https://github.com/openai/codex/blob/53eaa297e595fc98df0f33d4c63686a7014d7c9a/codex-rs/ext/goal/src/runtime.rs)
- [Goal 内部上下文构造](https://github.com/openai/codex/blob/53eaa297e595fc98df0f33d4c63686a7014d7c9a/codex-rs/ext/goal/src/steering.rs)
- [Goal 工具 schema](https://github.com/openai/codex/blob/53eaa297e595fc98df0f33d4c63686a7014d7c9a/codex-rs/ext/goal/src/spec.rs)
- [Goal 工具处理](https://github.com/openai/codex/blob/53eaa297e595fc98df0f33d4c63686a7014d7c9a/codex-rs/ext/goal/src/tool.rs)
- [Goal continuation 模板](https://github.com/openai/codex/blob/53eaa297e595fc98df0f33d4c63686a7014d7c9a/codex-rs/ext/goal/templates/goals/continuation.md)
- [核心压缩实现](https://github.com/openai/codex/blob/53eaa297e595fc98df0f33d4c63686a7014d7c9a/codex-rs/core/src/compact.rs)

本机证据：

- `codex app-server generate-json-schema` 生成的 Goal/Turn 协议 schema。
- `~/.codex/goals_1.sqlite` 的只读 schema。
- 上述 archived rollout 的 `task_started`、`task_complete`、
  `context_compacted`、Goal 工具调用和 Goal 完成返回。
