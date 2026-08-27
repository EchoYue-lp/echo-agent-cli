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
- Claude Code 的 official common-workflows/checkpointing 文档链接已记录在仓库总纲。本轮执行环境
  访问 `code.claude.com` 连续超时，因此没有把未实时取得的页面内容作为新增事实。

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
6. AgentRouter 每个 target 使用长寿命 `SegmentedFileEventJournal + CheckpointedReducer` authority。
   持久化 FIFO frontier 让 enqueue/claim/settle 使用单事件局部校验；完整 validate 只在
   open/recovery 和显式 records 诊断执行。hot projection 在 256 个 terminal 和 256 KiB terminal
   payload 两个上限内保留最近前缀，并始终保留全部 frontier；records 返回这一 retained window。
   message_id 幂等/碰撞保护同样以 retention window 为界；旧 identity 一旦因 count 或 byte 上限
   被淘汰即可立即重新进入。单条超出 terminal byte budget 的记录结算后立即从 hot projection 淘汰，
   active frontier 的完整输入不受该限制。
   framework 拥有 sequence、torn-tail、prepared-batch lookup 和 durability；degraded commit 不重试，
   OutcomeUnknown 必须 reopen + lookup 后才继续。checkpoint 后只 replay bounded suffix，并清理旧 segment。
7. Conversation/workspace 删除使用 cancellation-safe retirement guard：先关闭 admission、等待已接受
   mutation、删除 journal/checkpoint，再删除其它产品 authority；同 identity 重建得到空 inbox。

## 影响

- 不引入 SQLite，不建立 Task/Plan/Subagent 之外的平行执行概念。
- 不同 target 不再被一个进程级 async mutex 串行。
- terminal checkpoint、内存占用和显式 records 都受固定窗口约束；完整历史不作为 EKO inbox 的
  长期查询契约。
- `EffectStarted` 表示可能开始副作用，`Drained` 表示 framework 已确认进入模型上下文。
- 已消费但结果不确定的消息需要用户检查后重新发送；这比重复执行本地副作用更安全。
- GUI、TUI、CLI、JSONL 和 channel 继续共享 app-core authority，不新增 surface 特例。
