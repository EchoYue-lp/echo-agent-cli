# ADR 0004: Application Lifecycle Supervisor

- Status: Accepted
- Date: 2026-08-24

## Context

EKO 的 GUI、TUI、CLI/JSONL 和 channel 共享同一批进程级资源，但历史入口分别维护关闭清单。
局部 owner 已经能正确关闭自己的资源，例如 foreground turn、TaskRun driver、AgentPool、
Agent delivery、Plugin、MCP 和 Browser；缺失的是统一的应用级 admission、取消、join 与错误回执。

旧顺序还存在一个可达死锁：Agent delivery 在 live steer 后等待 foreground settlement，GUI/headless
却先等待 delivery supervisor，再关闭 foreground。活动 turn 不结算时，两边永久互等。bootstrap
也会在 Browser prewarm 后继续执行可失败步骤，失败路径没有覆盖所有已启动资源的渐进式 owner。

## Options

1. 继续由每个 surface 维护关闭顺序。改动小，但会继续产生行为漂移、漏 owner 和不同错误语义。
2. 把 EKO 生命周期下沉到 `echo-agent`。可以共享代码，但 workspace、surface hook 和本地产品资源
   都是 EKO 策略，会污染通用框架。
3. 在 app-core 增加薄的 application lifecycle owner，组合现有 subsystem owner，并由所有 surface
   使用同一种 typed receipt。

## Decision

采用方案 3。

`ApplicationLifecycleOwner` 在 `AgentRuntime::bootstrap` 成功后的下一行建立，并随着 TaskRuntime、
AgentPool、AppState、config watcher 和 surface bridge 成功创建而渐进绑定。任何中间失败都由当前
owner 执行 `BootstrapRollback`，不会依赖调用方猜测哪些资源已经启动。

关闭分为两个明确阶段：

1. `begin_shutdown` 同步关闭所有应用 admission，并广播 root/subsystem cancel，不等待任何任务。
2. surface 完成必要的 session-end hook 后，`join` 等待已接受的 owner；一个失败不会跳过后续 owner，
   所有错误进入 `ApplicationLifecycleReceipt`。

`join` 由内部 settlement task 持有 owner，并通过共享 receiver 发布回执。调用者丢弃或取消自己的
wait future 只会停止等待，不会丢失实际 shutdown。`AgentPool` 在释放 pool 锁前取得同一 admission
authority 的 execution receipt；Tauri bridge 在 spawn 前取得 reservation。phase one 关闭 admission
后不能产生未被 phase two join 的新任务。

Agent delivery 的 live settlement wait 必须同时监听 supervisor cancel。取消后 `Injected` 保持
非终态并留给下次启动恢复，不能伪造 `Delivered`。delivery driver 使用 RAII 清理 active target；
panic/cancel 的 join failure 通过 Tokio task ID 关联 target 后进入聚合回执。active/dirty owner 带
generation；旧 driver 的延迟 Drop 不能清理新 owner，dirty driver 异常退出会通过 durable inbox
既有 wake path 重启。

设计参考 Tokio graceful shutdown 的通用模式：`CancellationToken` 负责取消广播，任务集合负责
等待完成。EKO 继续复用这些原语，但 application admission、surface 顺序和产品回执留在 app-core。

## Consequences

- GUI、TUI、CLI/JSONL 和 channel 使用相同的 shutdown/rollback 语义。
- Desktop 启动或运行失败在写诊断后继续返回 `Err`，不会向 launcher 报告成功。
- subsystem 仍是自身生命周期的唯一 authority；application owner 只负责顺序和聚合。
- 新增进程级资源必须在创建成功后立即绑定 owner，并提供无等待的 phase-one cancel/close 与可等待的
  phase-two settlement。
- `echo-website` 和 framework examples 不受影响；这是 EKO 应用内部生命周期，不改变公开框架 API。
