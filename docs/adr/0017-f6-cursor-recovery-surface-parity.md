# ADR 0017：F6 Cursor、恢复与 Surface 对等验收闭环

## 背景

F1-F5 已经收敛 Conversation input receipt、Task/Plan/Todo authority、Agent control tools、
InteractionMode 删除和 Agent/Subagent 生命周期。F6 的底层机制也已经存在，但此前只有分散
单元测试和静态 surface matrix，没有一组可执行合同同时证明：cursor 跨进程恢复、cold address、
workspace incarnation、boot reconcile、terminal exactly-once，以及 GUI/TUI/CLI/JSONL/channel
使用同一持久事实。

本阶段不新增架构或公共 API。ADR 0011 已选择 framework journal + EKO product policy，ADR 0016
已选择 `AgentControlService` 薄 adapter；F6 只关闭这两个既有决策的验收缺口。因此没有重新设计
cursor、状态机、store 或 surface reducer，也不需要再做一次关键架构选型调研。既有决策参考的
OpenAI Codex Thread/Turn/Item 事件边界和 Claude Code durable identity/checkpointing 仍是本阶段的
依据：accepted、drained 和 terminal 是不同事实，客户端不得从自然语言或 transport EOF 推断完成。

## 全仓库盘点

| 能力 | 唯一权威 | 结论 |
| --- | --- | --- |
| Conversation Agent wait cursor | `AgentRouter` target journal sequence，经 `AgentControlService` 绑定 target token | live，跨 router reopen 可恢复 |
| TaskSubagent wait cursor | `TaskRuntimeStore` event sequence，经 exact attempt target token 绑定 | live，跨 store reopen 可恢复 |
| multi-target wait | `AgentControlService` target-prefix -> sequence 的有界 cursor map | live，不新增第二 wait owner |
| TaskRun boot recovery | store-scoped `TaskRunBootReconciler` success-only singleflight | live，重启后从 TaskRuntime journal 恢复 |
| ordinary chat replay | `ChatEventLog` cursor | live；它是 turn event stream，不是 Agent-control wait 的重复 authority |
| channel input lifecycle cursor | `ConversationInputService` queue revision | live；它跟踪 transport ingress receipt，不是 Conversation Agent inbox cursor |
| GUI/TUI/CLI/JSONL/channel | canonical `ChatDriverEvent` / `ChatEventEnvelope` | runtime renderer 各异，但没有独立 terminal authority |
| 原静态 surface matrix | prose-only test scaffolding，已删除 | 同一 durable fixture 的可执行 replay 归属 `f6_contracts.rs` |

盘点没有发现第二套生产 Agent wait store、boot reconciler 或 surface terminal 状态机。旧 F0
characterization 中“cursor wait 尚待后续”的注释已经过期；保留其对
`SubagentControlService` 不拥有 cursor/wait 的断言，并改为明确 cursor/wait 属于上层
`AgentControlService` adapter。

## 决策

1. Conversation cursor token 继续由完整 target identity 的 SHA-256 前缀和 router journal
   sequence 组成。重启后重新打开相同 router root，token 必须保持不变；重复提交同一 retained
   `message_id` 返回原 terminal receipt，不追加第二个 terminal。
2. TaskSubagent cursor 继续绑定
   `workspace_id/run_id/task_id/plan_revision/execution_id/attempt/workspace_generation`。重启后重新打开
   同一 workspace TaskRuntime，旧 cursor 只交付其后的 exact-attempt event；再次等待必须 timeout，
   不能重复 terminal。
3. Cold Conversation 由绑定 workspace 的 `ConversationStore` 证明存在；router inbox 可以尚未加载。
   workspace switch 使用 workspace-scoped service fail-closed，delete/recreate 使用新的 opaque
   generation 使旧 target fail-closed。不得从当前 GUI/TUI focus 推断 target。
4. boot reconcile 继续是 store-scoped success-only singleflight。第一次进程重启把 incomplete run
   收敛为 boot-paused；第二次进程重启不得再次恢复同一 run，也不得留下 active Subagent boundary、
   run-driver receipt 或 TaskRuntime operation。
5. GUI/TUI/CLI/JSONL/channel 以同一个持久化 `ChatEventEnvelope` fixture 重放 running/completed，
   五端合同只接受一个 typed completed terminal。renderer 可以改变呈现，但不得从 final answer、
   文本关键字、transport EOF 或 surface-local flag 生成第二终态。
6. Agent control bounded query 仍由 ADR 0016 和其底层 API 负责；F6 测试只消费该接口，不在 adapter
   或测试 fixture 中实现第二套 history scan。

## 影响

- 不引入 SQLite，不新增框架 API，不新增应用 store，不改变 GUI/TUI/CLI/channel 产品能力。
- `AgentRouter`、`TaskRuntimeStore`、`ChatEventLog` 和 `ConversationInputService` 的 cursor 分别绑定
  不同持久事实；名称相似不代表重复 authority。
- F6 验收直接检查 process generation 边界和 exactly-once terminal，而不是把源代码 grep 或静态
  matrix 当作恢复验证。
- surface fixture 验证共同 durable boundary；各 renderer 的排版、分块和交互行为继续由各自 focused
  tests 验证。

## 验证

`echo-agent-app-core/src/f6_contracts.rs` 覆盖：

- Conversation terminal cursor 跨 `AgentRouter` reopen、duplicate terminal 幂等、无 in-flight claim；
- TaskSubagent cursor 跨 `TaskRuntimeStore` reopen、exact release 单次交付、无 active boundary/receipt；
- cold/unloaded Conversation address、foreign workspace fail-closed、delete/recreate generation rejection；
- disk boot reconcile 跨两次 process generation 只恢复一次；
- `f6_contracts::interactive_surfaces_replay_one_canonical_fixture_without_terminal_inference`
  使用同一持久 fixture 验证 GUI/TUI/CLI/JSONL/channel typed terminal 对等，并可按完整测试名直接运行。

提交前运行 focused F6 tests、surface contract tests、`cargo fmt --all -- --check` 和相关 app-core
check；最终 workspace/all-feature 门禁由本轮总集成冻结点统一执行。
