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
   近期展示投影仍有界；本事件不证明主模型已经理解或执行，也不要求先写独立语义Applied账本。GUI/IM 的来源接受按下节顺序先持久化，
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
仍是同一原事件 union 的历史事实，不新增状态机。以下Plan14来源与原接受/上下文证据保留历史；当前普通Steer理解与Goal权限以文末2026-10-27裁决为准。

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
   成功后的近期记录保持原路径；来源接纳不代表已经进入主模型上下文或已经执行。
   普通Steer由主模型结合历史理解，不为旧Plan14建立独立语义Applied或持续要求状态机。
   完整原文恢复沿下文的同journal来源族与Plan15验收，不自动改Goal、计划或验收。

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


## 2026-10-27 裁决：Steer 由主模型理解，已有 Goal 仅用户可改

用户明确：普通Steer由主模型结合历史理解并在执行中按需更新计划，不先调用独立模型，
也不强制串联“有效要求更新 → 计划调整 → 验收调整”。该裁决替换旧Plan14的独立理解和
强制传播承诺，旧原文修复、接纳、幂等、错误与恢复证据保留各自真实范围，不改称全部通过。

TaskRun.goal是必填字段，未建立该运行目标时复用原初始化设置初始Goal。已有Run的重试、
ensure_scope与create_run重入返回原目标；普通Steer接纳/消费和模型task_update不能改写已有
Goal、goal_revision或goal_hash。用户通过GUI目标编辑或IM `/task-goal` 显式修改，继续使用
原RunGoalActorSource、expected_goal_revision/CAS和安全点；用户目标变更的原版本失效规则保留。

用户“结束并删除当前目标”沿原结束/取消入口退出活动目标呈现，不向后端写空Goal或抹去
历史事实。普通执行取消同样按原生命周期结算并保留goal/revision/hash，不能把取消运行等同
修改或历史删除Goal而另加权限门控。原Stop、HITL、恢复与物理结算owner不变。

必要历史、完整长原文及末尾限制继续沿原journal、压缩投影与read_artifact读取；缺失或
校验失败时原required_context_unavailable暂停相关新副作用，原Goal和可恢复owner保留。
这里不新增GoalStore、要求解释器、第二份队列/状态机或结构化模型协议，也不裁剪通用框架/SDK API。

## 2026-10-27 原始输入历史的同源恢复

普通Steer继续由主模型结合历史理解并按需调整计划；原文恢复不引入另一模型、语义分类器或自动Goal更新。原`events.jsonl`仍是唯一输入事实源，`HistoryProjection`在原artifact/review族之外增加原始输入来源族，保留`RunInputAccepted`与`RunSteerRecorded`各自的原始记录与序号。稳定输入身份沿原幂等规则处理；相同文字、相同turn的合法重复不按字符串合并，Accepted也不代表已理解或已执行。

每次投影在原RunAuthority及产品数据lease下固定journal identity与序号前缀；来源族派生文件和现有`InputResourceRef`把该前缀提供给主模型，主模型沿原`read_artifact`分页读取必要的早期原文。近期四条只是紧凑呈现，不再充当全部来源分母。追加的新输入产生新引用；相同来源字节不会因无关工具事件或时钟变化重写文件。不同Run、工作空间或journal代际不能挪用旧引用。

最终模型请求资格校验核对其实际收到的引用及原工具正上界；原始来源、SHA或原reader不可用时沿既有`required_context_unavailable`暂停并保留原owner/Goal，修复后沿原expected-resume恢复。派生缓存丢失或损坏可从准确journal重建；若重建改变文件身份，旧cursor明确拒绝，从无cursor重读，不能以空历史继续。路径来自本机产品data-root，即便项目绑定SSH也不调用远端文件系统。

本节是源码接线合同；定向测试、完整矩阵、新Tauri与真实GUI/IM验收分别报告，不因文档或局部通过宣称全部完成。

### 长Goal的本机读取与短Goal正控

canonical Goal仍只由原TaskRuntime及用户CAS修改。超过既有Goal内联/溢出边界时，完整当前Goal通过原materialize_recorded_user_text发布到本机artifacts根，模型得到带Run、journal代际、goal_revision与原SHA的不可变引用；原tasks/objective文件仍作归档，不再冒充host read_artifact入口。legacy初始化没有RunInputAccepted也从原canonical Goal事实生成该引用，不伪造输入ID或Applied。旧已发布Goal文件保持不变，新Goal生成新引用；旧目标引用不能作为当前版本执行依据。

短Goal仍按原方式内联；一个有界初始接纳原文可在来源引用中直接呈现，近期短Steer继续原有界呈现。最终请求只有在必要全文实际未呈现时才要求reader，不因短Goal或已完整可见的原文给NoTools/read_file-only任务新增工具门控。这只是核对实际字节可见性，不判断用户意图、不改Goal或计划。长Goal或早期原文实际省略且缺host reader时，仍走原required-context暂停与恢复，不扩大工具上界或用SSH read_file替代。

完整可见性的判断以最终实际模型请求为准：既有 GoalContract 能内联的边界与未启用 continuation 时的恢复预览边界分别沿用；预览会省略的 canonical Goal 发布 host 引用。当前 PreparedUserTurn 已完整内联的中段文字（超过预览长度但未到 spill 边界）保持 NoTools／read_file-only 正常执行。此前真实配对 read_artifact 调用和 Tool 结果若仍完整留在请求中，按分页字节范围、全文覆盖及原 SHA 核对后也算全文可见；缺页、重叠不一致或 SHA 不符不算。核对不会从本地文件补入缺失模型文字，也不把“文件存在”当作“模型已读取”。同一 Goal 的已发布引用资格仍绑定 Run、journal、revision 与 hash，不随当前投影显示模式改变。
