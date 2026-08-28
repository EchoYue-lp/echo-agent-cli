# RuntimeTaskService 适配决策

状态：已采纳

## 背景

EKO 曾在应用层同时维护 DAG frontier、claim、retry、pause/cancel 和 descendant block，
而 `echo-agent` 已提供 revisioned Task graph 与相同的通用执行语义。两套权威会使 stale
claim 覆盖新 attempt、暂停消耗 retry、依赖阻塞长期写入 journal，也使 GUI/TUI 看到的
状态与真实调度状态不一致。

## 候选方案

1. 保留 EKO executor，只复用 framework 数据类型。迁移最小，但继续存在双状态机。
2. 把 EKO review、worktree、文件字段和 UI 状态全部下沉 framework。只有一个 executor，
   但会用本地个人助理策略污染通用框架。
3. framework 拥有通用 task service；EKO 提供薄 adapter 和产品策略。本项目采用此方案。

## 决策

`RuntimeTaskService` 是 production DAG 的唯一构造入口。分层如下：

- framework 通用机制：DAG 校验与 ready frontier、exact physical claim、revision safe point、
  retry/requeue、terminal settlement、pause/cancel disposition、derived dependency state。
- EKO 产品策略：review、worktree integration、文件 ownership、unattended preflight、run
  pause/cancel intent、GUI/TUI/CLI/channel 投影。
- adapter：无损转换 `PlanTask` extension，调用 framework pure transforms，并把 task 变化、
  EKO summary 和 claim-bound review 作为一个 `append_batch` 提交。

`events.jsonl` 是 EKO 权威。`plan.json`、run state 和 Todo 是可重建投影。`plan.json` 只保存
`PlanRevision` specification，run state 保存无损 framework `TaskStatus`；Todo 只能由两者投影，
不能反向驱动调度、completion 或 recovery。projection refresh
降级发生在 journal 已提交之后时，调用方保留 typed mutation outcome，后续读取自愈；
batch outcome unknown 则 fail closed。依赖失败只通过 `DagExecutionState` 派生到 Todo read
model，绝不写 descendant `Blocked` 事实。

TaskRuntime executor 中两类有限 primary-Agent turn（独立 run 的规划 turn，以及 PlanTask
回落到 primary Agent 的执行 turn）统一交给 framework `AgentTurnDriver`。framework 独占 raw stream 启动、
envelope sequence、exact terminal、typed failure、cancellation 和 provider-reported usage receipt；
EKO 只有一个 `EkoAgentTurnSink`，负责 `ExecEvent`、event-id usage、tool start/terminal、evidence
与 artifact 投影。流在没有 terminal event 时必须失败，不能再以“stream 已结束”推断成功。

RunTurn terminal 的应用权威位于 `task_runtime/turn_lifecycle.rs`。chat、background/cron 和
`create_complex_task` 的 owned driver 都先提交同一 `RunTurnFinished`，再由同一 service 决定
provider retry、budget、cancel、pause 或下一 turn。Owned driver 不调用 detached
`request_continue`，也不等待自己的 driver-idle；它通过 `await_owned_continue` 复用同一
eligibility、durable retry deadline、shutdown/cancel 和 command-cell wake source，并在
Deferred 期间继续持有原 registration，因此不存在 handoff 空窗或第二套 active state。
Lifecycle 的 journal 读写全部通过共享 `TaskRuntimeBlockingAdapter`；pool/configuration 在
model 启动前被拒绝时也只调用该 service，不再在 chat driver 复制 finish/pause/cancel/cell
policy。

所有 RunTurn claim 的 run 必须在创建事务中显式配置 continuation。Provider-reported usage
按 envelope event id 写入 active RunTurn；未报告的 usage 不会被记为零成本事实。Primary
PlanTask 的 typed `AgentFailure` 作为 durable Subagent evidence 进入 exact-claim settlement；
retryable LLM failure 使用稳定 fingerprint requeue，并把 retry exhaustion 的 `Failed` /
`TimedOut` 分类显式交给 framework；direct timeout 和最终 exhaustion 都由 framework 在同一
CAS 中提交为 `TaskStatus::TimedOut`。EKO adapter 不再先结算 `Failed` 后改写状态，也不根据
product metadata 推断 terminal event。
Task-level Completed/Failed/TimedOut/Cancelled/Blocked live projection 只在 exact claim CAS
提交后发布；physical Subagent attempt 可以先发布自己的 terminal，但不得冒充 PlanTask terminal。

`create_complex_task` 创建的 attended 独立 run 会把当前 surface 的 `HumanLoopProvider` 作为
值传入 run-scoped pooled Agent，并保留已有 approval cache。Unattended run 不注入交互 provider，
因此 cron 或其它 unattended 独立 run 不会等待一个不存在的用户输入 owner。该差异是 EKO 产品策略，
不进入 framework turn driver。

暂停与取消遵循 `Cancelled > Paused`。EKO 不新增 task-level Paused UI 状态：active task 的
framework Paused 在文件 adapter 投影为 unclaimed Pending，Run 保持 Paused；resume 不增加
retry。取消先持久化 Run/Task Cancelled，再触发 driver token。

Resume 只有两个权威 intent，二者都携带从 journal projection 捕获的
`TaskRunResumeIdentity`：

- 已有 Plan 的普通 run 通过共享 `launch_planned_run_resume` 恢复同一
  `RuntimeTaskService` executor；GUI/TUI/CLI/channel 不自建 launcher。
- long-horizon chat 通过 `resume_and_claim_run_turn_expected` 在同一物理 batch
  提交 resume 事实与 `RunTurnStarted`。

identity 包含已 fold 的 journal sequence，因此 pause ABA、附件或 continuation 变化都会使
旧操作失效。CAS 未提交时只拒绝 driver registration，不得把当前 run 改写为
Failed/Cancelled；append 结果不确定时必须重开 journal 并对账。

RuntimeTaskService executor 和共享 planned-resume adapter 的异步文件 I/O 通过固定上限的
`TaskRuntimeBlockingAdapter` 进入 `spawn_blocking`。进程 semaphore 只限制并发；每个 store 的
operation supervisor 持有 blocking handle、RunDriver、CommandCell observer、多阶段 Subagent
command 和 turn projector，调用方只等待 receipt。caller future Drop 不会 detach operation；
Application phase one 关闭 global/workspace 新 admission，已接受 owner 通过 nested capability
完成 terminal settlement，phase two 再限时 join 并把失败写入 lifecycle receipt。reservation 与
admission seal 在同一锁内线性化；CommandCell 在 framework prepare 前、RunDriver 在 owner 发布前
预留 operation，因此 join 返回后不能出现 late accepted operation。phase one 还启动 framework
command manager shutdown，先取消长命令再 join；terminal repair 有上限，耗尽形成 typed
projection debt，observer join 有总 deadline。active operation 同时阻止 workspace eviction。
Workspace 通过 idle proof 后先提交 Closing，再执行不可逆 shutdown；host 原子 claim 并启动唯一
state-owned shared settlement。eviction caller drop 不会取消 owner 或丢失已消费的 debt。
cleanup degraded 的 generation 留在 registry 中保持 sealed，后续 control/open 与二次 eviction
不能重新开放它或吞掉原始 debt。framework command shutdown 是可重复 await 的 shared result，
并发 caller 不会覆盖 owner。

ChatEventLog 的 async safe point 通过 bounded product-data adapter，并让 blocking closure 捕获
exact workspace I/O receipt。manual compression、GUI cancel 和 HITL orphan projection 在 caller
drop 后仍持有 generation；workspace delete 不能先删文件再被迟到 append 复活。

排队 resume 的 identity 冻结 continuation route。TUI/CLI 不在 surface 读取 run snapshot 或严格
预判 journal sequence；真正判定统一由 store 原子 resume transaction 完成。identity 后只有
`execution_path` diagnostic Note 的 suffix 可以继续，其它状态、附件或 continuation 变化仍拒绝。

chat preparation/projection、background submit/control/query、boot reconciler、revision adapter、
Subagent control、GUI/TUI/CLI/channel TaskRuntime surface 和 unattended worktree control 都复用
该边界。已接受的复合文件 mutation 把 driver registration 随 closure 一起持有，因此 caller
drop 不会提前释放 workspace generation。

Tauri mutation 使用 ts-rs 生成的 `TaskRunControlReceipt`、`TaskRunResumeReceipt` 和
`TaskRetryReceipt`。Continuation resume 的 `turn_id` 是正式 wire 字段；surface capability
来自已注册工具与 workspace facts，不再由前端传递 mode DTO。

## 取舍与影响

- EKO adapter 不再拥有 executor、validator、retry loop 或 descendant traversal。
- EKO adapter 不再拥有 raw Agent stream loop；普通 background 与 boot 恢复后的 PlanTask 执行
  通过同一 `AgentTurnDriver` 路径，并共享相同的 terminal/usage/tool 投影约束。
- Direct completion 保留已观察到的只读 evidence、verification 和 artifact；成功的直接文件
  写入仍不满足 fixed direct-summary contract，必须走正式 writer PlanTask。
- Independent planning Agent 在 invocation 前把完整 registry（含动态 plugin/MCP）的
  Write/Execute tool 加入 `disabled_tools`，producer handler 因此不会执行；sink 同时按 canonical
  tool identity fail closed。动态 Read tool 保持可用。
- 当 primary RunTurn 仍 active 时，Task graph quiescence 不单独写 Goal Completed。Goal
  `RunStatusChanged(Completed)` 与 `RunTurnFinished` 在同一 journal batch 提交；direct summary
  graph 也先保持 Running，交由同一 terminal batch 完成，因此不存在 Completed + active-turn
  crash window。
- framework trace 没有 Paused 变体，因此 EKO 不写该可选诊断记录，绝不把 Paused 伪装成
  Completed。
- Review、summary 和 touched-files extension 只有 exact claim CAS 成功后才发布；stale claim
  的候选结果直接丢弃。
- Subagent durable recovery 使用 revision、attempt、physical execution id 和 TaskStarted
  线性化边界；合法的 restart handoff 可复用，迟到旧 release 不可越过新 claim。
- Async surface 不再直接执行 TaskRuntime file I/O；source inventory 固定所有 production
  surface、boot、Subagent、projector 与 lifecycle/workspace owner 都必须经过同一 adapter 和
  store operation supervisor。
- Projection refresh 已返回 committed/degraded typed outcome；Todo、latest Summary 与 Completion
  Gate 使用 checkpoint-backed projection，Artifact/Review 历史使用增量 segment。10k/100k
  release 规模验证仍属于 Final Integration Gate，不能仅凭实现存在宣称已通过。

## 业界依据

- [Temporal Workflow execution](https://docs.temporal.io/workflow-execution)以 event history
  恢复 durable execution，并把重试策略与业务结果分开。
- [Temporal Retry Policies](https://docs.temporal.io/encyclopedia/retry-policies)把 retry 作为
  明确策略，而不是由任意状态写入隐式推断。
- [LangGraph Persistence](https://docs.langchain.com/oss/python/langgraph/persistence)使用
  checkpoint/thread identity 恢复 graph。
- [LangGraph Interrupts](https://docs.langchain.com/oss/python/langgraph/interrupts)在 durable
  checkpoint 上恢复，而不是把暂停当失败并增加 retry。

这些实现共同支持本项目的取舍：持久事实与可重建投影分离、恢复必须绑定稳定 identity、
暂停和 retry 是不同语义、产品 policy 留在 adapter 边界。

## 不适用范围

`echo-agent-examples` 用于验证 framework public facade，不应引入 EKO 文件 journal、review
或 worktree policy。`echo-website` 是静态展示站，不依赖 agent runtime。因此本决策不修改
framework examples 或 website。
