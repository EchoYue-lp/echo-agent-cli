# ADR 0011: Boot 与 Agent Inbox 恢复权威

## 背景

EKO 的 TaskRun、普通 Chat command cell、Awaiter 和跨会话 Agent 消息都需要在进程重启后
继续收敛。旧实现存在四个不同问题：TaskRun recovery 永久缓存瞬态失败；启动扫描跨文件 I/O
和 retry 持有 workspace transition 写锁；AppState 与 BackgroundTaskService 会同时启动 background
run；AgentRouter 把 mailbox acceptance 当作消费完成，并在每次事件追加时重写整个 JSONL。

本决策只定义 EKO 的产品恢复策略。通用 steer lifecycle 和 append-only journal 继续由
`echo-agent` 提供。

## 参考实现

- OpenAI Codex app-server 把 `turn/steer` 接受请求、`item/completed` 工作结果和
  `turn/completed` 根 turn 终态分成独立边界；恢复使用稳定 thread/turn/item identity。
- OpenAI Codex `exec_events.rs` 对外提供稳定的 `thread.started`、`turn.started`、
  `turn.completed/failed`、`item.started/completed` 事件，而不是让客户端从瞬时调用返回值推断完成。
- Claude Code Agent Teams 用持久 shared task list 保存 pending/in-progress/completed，而不是只从
  当前活跃 teammate 推断任务是否存在；resumed session 继续使用该 task list。
- Tokio `JoinHandle` 保留 task completion 并允许稍后 await；`&mut JoinHandle` 在 `select!` 中
  cancel-safe。Python `asyncio.Future` 同样在 done 后保留 result/exception。两者都不要求观察者必须
  在异步工作仍 active 时注册。
- Kubernetes API 用 list/get 返回的 `resourceVersion` 接续 watch，明确闭合 snapshot 与 live stream
  之间的窗口。Claude Code Agent Teams 的 shared task list 保留 pending/in-progress/completed 状态，
  并跨 resumed session 保留 task list。

官方参考：

- <https://docs.rs/tokio/latest/tokio/task/struct.JoinHandle.html>
- <https://docs.python.org/3/library/asyncio-future.html>
- <https://kubernetes.io/docs/reference/using-api/api-concepts/#efficient-detection-of-changes>
- <https://code.claude.com/docs/en/agent-teams#assign-and-claim-tasks>

跨系统的共同点是：accepted 不等于 consumed；恢复沿持久化事实和稳定 identity 前进；已开始但
终态未知的副作用不能盲重放。

## 候选方案

### A. 保留现状并增加 retry

改动最小，但仍保留双 launcher、长时间 workspace 写锁、Awaiter 提前 Ack 和 AgentRouter 全文件
重写，无法关闭 crash window。

### B. 在 EKO 新建完整 recovery 状态机

可以集中逻辑，但会重复 framework 已有的 `AgentSteerReceipt`、journal sequence 和 TaskRuntime
状态权威，形成第二套 authority。

### C. 复用 framework receipt/journal，EKO 只保留产品策略

TaskRuntime 继续以 `events.jsonl` 为事实；AgentRouter 使用 framework segmented journal；EKO
只决定 workspace 扫描、attended policy、reply/backoff 和 UI 投影。这是最终选择。

## 决策

1. `TaskRunBootReconciler` 只缓存成功 recovery。owner task 独立于首个 caller，caller abort 不会
   取消恢复；瞬态错误广播给同一轮 waiter 后回到可重试 Idle。TaskRuntime 文件操作经 bounded
   blocking adapter 执行。
2. boot 扫描不持 workspace transition 写锁跨文件 I/O、runtime 构造或 provider retry。
   AppState 负责普通 conversation continuation；BackgroundTaskService 是 global background run
   唯一 launcher owner。
3. Running 和已经 Paused 的 TaskRun 都修复 active command cell；没有 TaskRun 的普通 Chat cell
   由 ChatEventLog 恢复为一个 typed Interrupted terminal。Started 在 terminal 前固定 retention。
4. Agent delivery 在 live steer 或 cold driver 前先提交 `EffectStarted(actual_turn_id)`；
   framework receipt 到达 Accepted/Drained 后分别提交 `MailboxAccepted`/`Drained`。任一 owner-loss 窗口都保留原 attempt，
   但没有 EKO terminal 时一律 `outcome_unknown` 并禁止自动重放，不能用任意 assistant 文本冒充结果。
5. Awaiter 先写 Ready，再在 handoff 前写 DeliveryStarted；direct live steer 只在 Drained 后写
   Acknowledged，next-turn prompt projection 因没有 framework tracked drain 而保守写 `outcome_unknown` Ack。Ready、Started、
   Ack 都走 bounded blocking adapter。CommandCell service 在开放新 publish 前通过 success-only owned
   singleflight 收敛 boot cut 之前的 Started；per-turn projector 只等待该 readiness，禁止扫描本进程
   live Started。Ack 写失败由原 owner 重试；boot orphan 只收敛 typed `outcome_unknown` Ack，不再次注入。
6. `watch_cell` 以 durable TaskRuntime/Chat command-cell fact 校验 exact owner；process-local active set
   只管理 live resource。TaskRun 先校验 workspace/conversation/run，再用 durable cell `turn_id` 对 current
   turn；不能拿 immutable run root 代替 resume/continuation turn。普通 Chat 继续使用 conversation/root scope。
   durable owner read 前先取得不暴露 cell 内容的 framework observation lease，防止 terminal history 在
   disk snapshot 期间被 prune；owner 校验失败立即释放且不创建 receipt/dispatch/Ready。same-owner terminal
   在 watch 注册前已提交时，仍走既有 controlled Awaiter dispatch，复用
   retry-readable terminal snapshot，生成真实 Subagent summary 和 typed Ready，不重跑 command cell。非终态
   snapshot 若发现 live owner 已释放，必须二次读取 durable fact；只有二读仍非终态才报 exact-scope error。
   这样闭合 snapshot/live TOCTOU，又不引入第二个 store、terminal reducer 或伪造的 Awaiter Completed。
7. AgentRouter 每个 target 使用长寿命 `SegmentedFileEventJournal + CheckpointedReducer` authority。
   持久化 FIFO frontier 让 enqueue/claim/settle 使用单事件局部校验；完整 validate 只在
   open/recovery 和显式 records 诊断执行。hot projection 在 256 个 terminal 和 256 KiB terminal
   payload 两个上限内保留最近前缀，并始终保留全部 frontier；records 返回这一 retained window。
   message_id 幂等/碰撞保护同样以 retention window 为界；旧 identity 一旦因 count 或 byte 上限
   被淘汰即可立即重新进入。单条超出 terminal byte budget 的记录结算后立即从 hot projection 淘汰，
   active frontier 的完整输入不受该限制。
   framework 拥有 sequence、torn-tail、prepared-batch lookup 和 durability；degraded commit 不重试，
   OutcomeUnknown 必须 reopen + lookup 后才继续。checkpoint 后只 replay bounded suffix，并清理旧 segment。
8. Conversation/workspace 删除使用 cancellation-safe retirement guard：先关闭 admission、等待已接受
   mutation、删除 journal/checkpoint，再删除其它产品 authority；同 identity 重建得到空 inbox。

## 影响

- 不引入 SQLite，不建立 Task/Plan/Subagent 之外的平行执行概念。
- 不同 target 不再被一个进程级 async mutex 串行。
- terminal checkpoint、内存占用和显式 records 都受固定窗口约束；完整历史不作为 EKO inbox 的
  长期查询契约。
- `EffectStarted` 表示可能开始副作用，`Drained` 表示 framework 已确认进入模型上下文。
- 已消费但结果不确定的消息需要用户检查后重新发送；这比重复执行本地副作用更安全。
- GUI、TUI、CLI、JSONL 和 channel 继续共享 app-core authority，不新增 surface 特例。
