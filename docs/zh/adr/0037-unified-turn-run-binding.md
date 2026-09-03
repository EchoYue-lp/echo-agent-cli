# ADR 0037:统一 turn-run 绑定,每个轮次驱动一个 TaskRun

- Status: Accepted
- Date: 2026-09-03
- Owners: `chat_driver`、`tasks/task_runtime`

## 背景

ADR 系列与 2026-08-26 交互收敛计划删除了 `InteractionMode::{Chat, Task, Auto}`,
收拢为"一个入口、一个模式、一套运行时"。但 Iteration 4 当时规定"普通 turn 默认无
run;只有 task tool 或明确 scheduler/background trigger 创建/绑定 TaskRun",于是
当前实现中存在两类轮次:

- **绑定轮**(`binding.run_id = Some`):任务提交、resume、continuation——享有
  goal contract、恢复胶囊、journal、continuation 挂起-唤醒、boot 恢复等全部
  TaskRuntime 保护;
- **run-less 轮**(`binding.run_id = None`,普通用户消息默认):只有当 LLM 在轮内
  调用 `task_create` 时才懒升级为正式 run;此前与之后的轮次都运行在 TaskRuntime
  之外。

长程任务审计发现三个结构性缺口,全部源于 run-less 状态:

1. **裸对话无 goal 锚**:run-less 轮没有 `[eko_run_goal_contract]` 投影,早期用户
   意图滑出 SlidingWindow(40) 后只剩启发式记忆晋升兜底,压缩后不可恢复;
2. **懒 run 不跨轮**:turn N 内 `task_create` 建的 run 绑定 `taskrun:{turn N}`;
   turn N+1 派生的是另一个 id,goal contract 不自动延续;
3. **await/continuation 半覆盖**:轮次挂起-唤醒(`wake_after_cell_terminal` +
   `RunContinuationDeferred`)是 run 的属性,run-less 轮不享受。

"一套运行时"的产品定位下,run-less 轮本质上是第二条(更弱的)执行路径——它不是
功能差异,而是保护覆盖率的空洞。

## 业界参考与候选方案

本决策复用交互收敛计划对 Codex Thread/Turn/Item、Claude Code queued
message/task list 与后台任务的调研。Claude Code 会把运行中消息在工具结束后送入同一
turn,或在当前 turn 结束后作为下一 turn;task checklist 与后台执行视图彼此独立。
这些实现支持“统一 turn driver、消息可 steer/queue、plan 是 artifact”,但没有提供
“缺少 plan 就能反推 run 类型”的依据。因此 EKO 的 TaskRun 产品投影必须保存明确的
provenance 和 plan policy,不能从 route 文本或 plan 是否存在猜测。

- Claude Code: <https://code.claude.com/docs/en/interactive-mode#queue-messages-while-claude-works>
- 仓库调研快照:[Codex 能力目录](./0002-codex-tool-capability-catalog.md)、
  [Claude Code 能力目录](./0003-claude-code-capability-catalog.md)。

- **A. 补丁式保护**:为 run-less 轮启用框架的 VisibilityHorizon Global Objective
  或 IncrementalSummary durable anchors,让原始意图在压缩中幸存。
  否决理由:在统一运行时之外再造第二套轻量 goal 机制,两套投影、两套保真逻辑
  并存,与收敛目标相悖;且不解决缝隙 2、3。
- **B. 急切绑定(eager binding)**:默认 `RunTurnBinding` 携带
  `run_id = taskrun:{turn_id}`,每个轮次进入驱动即创建 TaskRun(goal = 用户指令),
  全部流量走同一 admission、投影、恢复语义。琐碎轮次(从未提交 plan revision)在
  turn 结束时直接结算为 Completed,不进 continuation 循环。

## 决策

采用 **方案 B**,并遵循以下约束:

1. **Typed provenance,不用 route 字符串**:`RunCreated` 保存
   `TaskRunExecutionProfile { provenance, plan_policy }`。只有
   `ConversationTurn + no plan` 是琐碎对话;`Orchestrated + AllowDirect` 在执行和恢复中
   可以合法地暂时没有 plan;
2. **结算规则**:`ConversationTurn` 的 `TurnOutcome::Completed` 且无 plan时,run 直接
   `Completed + Stop`;orchestrated run 继续遵守 `RequirePlan/AllowDirect`;
3. **噪音治理以 provenance 门控**:无 plan conversation run 不生成 progress.md 账本、
   `RunCancelledByUser` 不写长期记忆(`RunCompleted` 已有 completed-tasks 门控
   天然安全),GUI task query 也排除该投影;conversation run 一旦提交 plan,立即按正式任务显示;
4. **boot 恢复归类**:active conversation turn 原子结算为
   `Cancelled(Interrupted)`;已经持久化 `RunTurnFinished` 的 settlement debt 补为
   `Completed`;planless orchestrated run 保留 `Paused(BootRecovery)`;
5. **死代码清理**:run-less else 分支与 `register_optional_run_driver` 随之删除。
6. **用户 steer**:foreground owner 保存 exact `run_id`;GUI/TUI/CLI/channel 与
   AgentRouter 的 user-authored live delivery 共用 app-core 记录入口。Agent-origin message
   不伪装成 user constraint。
7. **quiet wake**:active task/cell 检查与 `RunContinuationResumed` 在同一 run lock 内提交;
   continuation eligibility 发现新 activity 时重新持久化 deferred。
8. **长 Goal artifact**:使用原子写,文件名绑定 goal revision 与 SHA-256,读取时复核 digest。

## 取舍

- **代价**:每轮对话产生一个 run 目录(events.jsonl 等),事件量与文件数增长;
  换取全流量有 journal 可审计、goal 保真与恢复语义全覆盖。若后续证明过重,优化
  方向是 run 分级存储,而不是退回 run-less。
- **行为变化**:`observe_execution_path` 将把普通聊天轮记为 formal_plan 路径;
  断言"ordinary chat must not create a TaskRun"的旧测试(收敛计划 Iteration 4
  退出门)随之改写。

## 影响范围

- `chat_driver.rs`(默认 binding、run-less 分支删除);
- `tasks/task_runtime/turn_lifecycle.rs`(provenance-aware 结算);
- `tasks/task_runtime/ledger.rs`、`memory_bridge.rs`(噪音门控);
- `tasks/task_runtime/store/runtime.rs`(typed provenance、原子 boot/quiet-wake、artifact,
  `register_optional_run_driver` 删除);
- `foreground_turn.rs` 与五个 surface adapter(exact run steer binding);
- `web-frontend` generated contract 与 task UI query;
- 顶层 `docs/2026-08-26-agent-interaction-convergence-plan.md` Iteration 4 修订。

## 关联

- 同分支后续实现:Subagent 终态唤醒轮次(泛化 `wake_after_cell_terminal`)、
  `RunSteerRecorded` 中途约束锚定、TaskExecutionSummary 引用化与 goal 超长落盘。
