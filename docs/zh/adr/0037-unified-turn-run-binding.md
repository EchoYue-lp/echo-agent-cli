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
   天然安全),GUI task query 也排除该投影;conversation run 一旦提交 plan,由
   `PlanRevisionCommitted` execution projection 立即激活正式任务显示;
4. **boot 恢复归类**:active conversation turn 原子结算为
   `Cancelled(Interrupted)`;已经持久化 `RunTurnFinished` 的 settlement debt 补为
   `Completed`;planless orchestrated run 保留 `Paused(BootRecovery)`;
5. **死代码清理**:run-less else 分支与 `register_optional_run_driver` 随之删除。
6. **用户 steer**:foreground owner 保存准确 `run_id`。原 `RunSteerRecorded` 保留完整原文，
   近期展示投影仍有界；本事件不证明语义要求已经应用。GUI/IM 的来源接受按下节顺序先持久化，
   然后才调用原 mailbox；TUI/CLI 与 AgentRouter 的来源身份接线仍是未迁移范围，不能把
   其旧文本记录冒充新接受回执。Agent-origin message 不伪装成用户约束。
7. **quiet wake**:active task/cell 检查与 `RunContinuationResumed` 在同一 run lock 内提交;
   continuation eligibility 发现新 activity 时重新持久化 deferred。
8. **长 Goal artifact**:使用原子写,文件名绑定 goal revision 与 SHA-256,读取时复核 digest。

## Plan14 第一片：原输入身份与任务持久接受

实现参考根内固定 ZCode `3c5b227649371507ddc4eb46ac5133afe6db1cab`：
`apps/zcode-cli/packages/core/src/runtime/methods/prompt-admission.ts` 的 reservation/guide/queue 与
`apps/zcode-cli/packages/bootstrap/src/zcode-protocol-v4/commands/prompt-turn.ts` 的准入、完成分离体现“原 runtime 按准确输入身份投递”的共性。
ZCode 普通 Bot callback
早于 sendText ACK 的窗口不作为本项目准入证明。EKO 的双原 journal 顺序、target-before-admission
和 typed unknown 是复用本项目原机制后的工程取舍；不是 ZCode 已有相同 API。`RunInputAccepted`
仍是同一原事件 union 的历史事实，不新增状态机；durable/context/applied 分离沿 Plan14 原要求。

本片沿已批准的 Plan14 复用现有 journal、输入 attempt 与统一 turn driver，没有新增
输入队列、要求 store、调度器或框架运行时字段。通用执行与取消仍归框架；来源快照、
任务去向及 EKO 的持久接受策略归应用。

1. 原 `ConversationInputFact::Persisted` 保存输入完整版本，原选择事务校验全队列
   `queue_revision`。取得准确 attempt 后，在同一 ChatEventLog 原锁下写
   `TaskRunTargetBound { attempt, run_id }`。绑定只核该输入的身份、修订、attempt、turn
   和已有去向；其它 DATA 入队不撤销已合法取得的 claim。
2. 绑定不是 TaskRuntime 接受，也不是 mailbox、Drained 或 Applied。任务日志新增明确
   `RunInputAccepted { role, input_snapshot }`；用途仅为 `InitialGoal` 或 `Steer`。
   首轮与原 `RunCreated` 在同一 journal batch 写入，Goal 使用原输入全文；模型继续消费
   `PreparedUserTurn` 的有界引用。追加输入只写来源接受事实，不能据此加入有效要求投影。
3. 只有原物理耐久结果 `Confirmed` 才准许后续投递。派生 checkpoint/展示修复失败与物理
   journal 未确认分开；同源 ID/修订重试核清原 frame，不重新追加。原 checkpoint 版本推进，
   旧 checkpoint 必须从同一 journal 重建来源去重索引。
4. 这是两个原 journal 的顺序写入，不是跨日志原子事务。去向已绑定但任务尚不存在时，
   只能用同一确定 ID 和原源版本重建；任务已结束、已编辑版本或归属不明时保留原输入并
   阻断自动改绑。已 claim 的旧记录若缺少任务去向，原 `ever_claimed` 重放事实保守报告
   `RecoveryRequired`，不猜当前或最新任务。取消/替换仍遵循原队列允许范围；绑定历史不抹除。
5. 正常队列删除不接受 `AttemptStarted`；这与低层 reducer 可恢复 Cancelled 事实的能力
   不同。用户 Stop 继续取消原准确 foreground token；旧 turn 的取消不能影响后继 turn。
   已启动工具由原物理 owner 结算，不能因输入取消假装副作用回滚。
6. `RunInputAccepted` 只建立历史来源索引，不填近期 `RunSteerRecorded` 投影。原 mailbox
   成功后的近期记录保持原路径；“进入上下文”“语义已应用”“持续要求取消/替换”与来源的
   精确关联仍属于 Plan14 后续切片。本片不宣称要求全集、自动 Goal/Plan 改写或完成门禁已闭合。

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
