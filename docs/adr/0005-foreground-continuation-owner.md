# ADR-0005：Foreground owner 覆盖完整 RunTurn continuation 链

> 状态：Accepted
>
> 日期：2026-08-24
>
> 范围：EKO GUI、TUI、CLI、channel 与 Agent surface 的 foreground admission、
> RunTurn continuation、steer、cancel 和 terminal settlement。

## 背景

一个长程 TaskRun 会由多个有限 RunTurn 连续推进。原实现由 foreground lease 只等待首个
RunTurn；`TaskContinuationRuntime` 随后另起 detached task。结果是首轮结束后 busy registry
已经释放，而第二轮没有 foreground root owner，使用新的 cancellation token，也无法让 surface
以稳定 root id cancel 或用当前 active id steer。

这不是 TaskRun 状态机缺字段，而是异步所有权边界提前结束。修复不得增加第二套 TaskRun、
Plan 或 completion 状态机。

## 参考实现

- Tokio [`task_local!`](https://docs.rs/tokio/latest/tokio/macro.task_local.html)要求通过
  `scope` 显式建立 task-local 作用域；独立 spawn 不能作为跨任务传播上下文的隐式契约。
- Tokio [`watch`](https://docs.rs/tokio/latest/tokio/sync/watch/index.html)允许多个 waiter
  订阅同一个最新 completion 值，适合表达一个 dispatch 的共享终态 receipt。
- Tokio [`CancellationToken`](https://docs.rs/tokio-util/latest/tokio_util/sync/struct.CancellationToken.html)
  的 clone 共享取消状态，适合让 root operation 与每个有限 RunTurn 使用同一取消权威。
- Tokio [`JoinSet`](https://docs.rs/tokio/latest/tokio/task/struct.JoinSet.html)提供 task ownership
  与 drain；EKO foreground supervisor 已使用它持有被接受的外层 owner。

这些机制共同指向同一个原则：spawn 是执行边界，不是所有权转移证明；可见 operation 必须有
一个持续到最终 settlement 的 owner，并通过 value-carried handle 把 identity 与 cancellation
传给子任务。

## 候选方案

### 方案 A：首轮结束即 settlement，continuation 完全后台化

改动最少，但 busy、stop、steer、workspace transition 和 renderer 生命周期都只覆盖首轮，
不满足 surface 功能对等和真实 operation 语义。

### 方案 B：每个 RunTurn 重新 begin 一个 foreground lease

每轮都有 owner，但前后两轮之间存在 registry admission 空窗；root identity、settlement waiter
和 cancellation token 被切成多段，也产生第二个 lifecycle authority。

### 方案 C：外层 lease 是唯一 owner，continuation 返回共享 completion receipt

外层 owner 等到 continuation Deferred 或 Stop。后续 RunTurn 只携带不可 settlement 的
`ForegroundTurnProgress`，在同一 entry 下更新 active id 并复用 root cancel。本项目采用此方案。

## 决策

1. `ForegroundTurnLease` 是一个 surface operation 的唯一 settlement capability。只有外层
   `drive_foreground_*` 能从 registry 移除 entry 和发布 terminal receipt。
2. `TaskContinuationRuntime::request_continue` 对首次请求返回 Started receipt；并发请求订阅
   同一 dispatch 的 watch channel 并返回 Joined。不得为 Joined 请求创建第二个 driver。
3. launcher 可在活跃链期间持有非 owning `ForegroundTurnProgress`。每个后续 RunTurn 显式
   `scope` 同一 entry，开始执行时更新 `active_turn_id`；`root_turn_id` 永不改变。
4. 后续 RunTurn 使用 progress 提供的 root `CancellationToken`。RetryAt、driver-idle 等待和
   模型执行均观察同一 token；root cancel 直到当前 driver 释放后才发布 foreground settlement。
   每个 active dispatch 还拥有独立 cancel capability；detached/recovery 没有 foreground token
   时，pause、cancel、删除和 shutdown 通过 `clear_launcher` 唤醒 RetryAt/idle wait，而不是只
   删除 launcher registry 后等待 deadline。dispatch 被唤醒后必须先 drain 该 run 的 exact
   RunDriver，再释放 launcher 并发布 completion receipt。
5. continuation runtime 自己创建的 Internal Continuation/Recovery turn 不得再次注册 launcher
   或调用 `request_continue`。原有 dispatch loop 是唯一 continuation coordinator。
6. Deferred 是当前 foreground operation 的终点，但不是 TaskRun 的终点。发布 Deferred receipt
   前，launcher 原子替换为 journal-only 或 discard sink，并释放 renderer、foreground progress
   和 root token。旧 launcher 在 continuation state 锁外完成 Drop，之后才能发布 receipt；
   background cell wake 后以 detached launcher 启动新 dispatch。
7. surface 使用 stable root id 执行 cancel，使用 snapshot 的 current active id 执行 steer。

## 锁与等待顺序

`ContinuationState` mutex 只保护 launcher、active dispatch 和 pending wakeup 的短事务；任何
`.await` 前必须释放。顺序为：

```text
continuation state lookup
  -> release mutex
  -> wait previous RunDriver idle / retry deadline
  -> TaskRuntime admission and claim
  -> foreground active-id update
  -> pool/model execution
  -> durable RunTurn settlement
  -> continuation state completion/detach transaction
  -> publish watch receipt
  -> outer foreground settlement
```

取消先写 TaskRuntime 的 run-level cancellation intent，再 signal 共享 token；外层 owner 等待
RunDriver release。Drop 不能 abort 这个唯一 owner，防御性 Drop 只请求 root cancellation。

## 影响

- GUI/TUI/CLI/channel/Agent 的 busy 与 workspace transition 覆盖完整 continuation 链。
- root cancel 可以命中第二轮及之后的 RunTurn，steer 使用实时 active id。
- renderer 不跨 Deferred 泄漏，journal 仍可记录之后由 background wake 启动的 detached turn。
- TaskRun、TaskPlan、RuntimeTaskService 和 continuation eligibility 仍是原有单一权威；新增
  completion waiter 只是 owner receipt，不是第二套业务状态机。

## 验证

- current-thread 测试证明两个同步请求得到 Started/Joined，并观察同一 completion。
- production driver barrier 在第二次真实 LLM 调用停住，验证 root 不变、active id 更新、
  第二轮 steer 命中、root cancel 命中、两个 settlement waiter 同值且不启动第三个 RunTurn。
- Deferred 测试验证 renderer 在 completion receipt 发布前已经释放。

## 文档与示例影响

这是 EKO 应用运行时所有权，不修改 `echo-agent` public facade 或 `echo-agent-learning`。
`echo-website` 不描述内部 RunTurn lifecycle，因此无需同步修改。
