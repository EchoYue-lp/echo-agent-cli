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

同步 product-data I/O 的生命周期按应用代际拥有：`AgentRuntime` 创建一个
`ProductDataIoService`，并无损注入 AppState、workspace host、CommandCell、analytics 与 Agent
research tools。进程级 semaphore 只负责容量。phase one 同时关闭 foreground/TaskRun/tool 等顶层
producer admission 与 ProductData 顶层 admission；新的 flow 和直接 I/O 都会 fail closed。已经接纳的
manual/delete/analysis/research/CommandCell/workspace flow 持 cloneable shared receipt，并只用其 nested
token 完成 provider/transform 后的 blocking safe point。phase two 等这些 flow 与 nested I/O 全部归零，
并聚合 caller 不再等待时留下的 durable failure debt。

Agent delivery 的 live settlement wait 必须同时监听 supervisor cancel。取消后已 `Drained` 的输入
保持非重放并由 owner 或 boot 写入 typed `TurnSettled`。delivery driver 使用 RAII 清理 active target；
panic/cancel 的 join failure 通过 Tokio task ID 关联 target 后进入聚合回执。active/dirty owner 带
generation；旧 driver 的延迟 Drop 不能清理新 owner，dirty driver 异常退出会通过 durable inbox
既有 wake path 重启。

设计参考 Tokio graceful shutdown 的通用模式：`CancellationToken` 负责取消广播，任务集合负责
等待完成。EKO 继续复用这些原语，但 application admission、surface 顺序和产品回执留在 app-core。

### 未结算执行的观察

`ExecutionNotSettled` 是对原执行的观察，不是失败或取消回执。共享 foreground、TaskRun 与
analysis owner 保留原 generation、pool、memory 和 I/O receipts；各 surface 只用既有错误呈现，
不发终态、不重放执行。driver 尚未启动时，已有的 live cancellation registration 也不能作为
已取消的证明。shutdown 报告未确认的所有权，不据此释放资源。

channel renderer 将既有 app-core observation 无损转回框架错误，保留 run/execution 身份。
GUI planned resume 先保留同一 foreground lease，再返回未结算观察；不得进入普通 Failed
结算分支。
launch 准备阶段遇到未结算的 durable owner fence 也遵循同一规则。foreground settlement
waiter 保留 typed run/execution 身份并传给 channel notifier；该观察不触发普通 input-pump kick。

共享 resume/recovery 的准备与 claim 也必须在 String 呈现之前保留既有 typed Store error。
重开存储后遇到未结算 owner fence，必须到达同一 foreground observation 分支；普通准入或
身份错误仍保留原上下文并按既有 Failed 方式结算。

接纳工作前，原 Task journal 或 analysis 目录内的私有 intent 记录原 owner 身份，重启准入继续
检查同一事实。只有匹配的物理完成才能记录 durable terminal 并清 intent。已完成但持久化失败
由同一 owner 串行重试原清理，不重新运行 Agent 或工具；无完成来源的 foreign unknown 保持 fence。


原应用 generation 根强持同一个生命周期 Owner 与原执行资源束；子系统索引与进度回边仅 Weak。真实清理 producer 先结算独立普通 flow/配额，再报告仍未结算的精确 owner，不关闭它依赖的 Agent/工具等资源。元数据观察不能伪作最终收据。原 typed 生命周期错误在 Runtime/Services 释放后仍持有该根；只有原 owner 的真实完成证据才推进同一清理并释放资源。opaque workspace I/O guard 仅保原计数，完整 scoped control 仍保 Host/Task/Tool 能力。

## Consequences

- GUI、TUI、CLI/JSONL 和 channel 使用相同的 shutdown/rollback 语义。
- Desktop 启动或运行失败在写诊断后继续返回 `Err`，不会向 launcher 报告成功。
- subsystem 仍是自身生命周期的唯一 authority；application owner 只负责顺序和聚合。
- 新增进程级资源必须在创建成功后立即绑定 owner，并提供无等待的 phase-one cancel/close 与可等待的
  phase-two settlement。
- `echo-website` 和 framework examples 不受影响；这是 EKO 应用内部生命周期，不改变公开框架 API。

ApplicationServices 的标准组装在 TaskRun 启动恢复或应用 scheduler producer 接纳工作前，先为借用 AppState 的 workspace registry、foreground turns 和 analysis supervisor 接入同一生命周期 root；最终 AppState Arc 通过同一 helper 接管强引用。冷 workspace store 准入原 driver 不得依赖 compose 已返回。


终态写入失败时，既有 settlement debt 保留原先准入并完成物理执行的 PhysicalOwnerIntent。投影缺失或读取失败均不能授权释放；重试在原 run 锁内验证同一 owner，再调用唯一 finalizer 和 acknowledgement，异步 receipt 释放放在锁外。执行前拒绝只能释放确证从未开始的资源。观察 watch 关闭不能覆盖已经发布的真实结算收据。原 driver 返回错误时，正常结算与读取错误兜底均从原取消 token 选择 Cancelled，否则 Failed；返回成功但未持久业务结算仍为 Failed。这是原应用 owner 的修复，没有增加生命周期状态或框架策略。

故障回归只在 canonical 物理完成后注入真实 I/O 错误，保留并恢复原 journal，要求恰好一次 acknowledgement 后才释放 receipt。冷恢复夹具区分 caller abort（原 owner 继续保留）与实际子进程退出并被回收；只重开后者证明没有模型重放，不伪造 owner 收据。针对性检查不构成原生 GUI/OS owner 晚恢复或完整 workspace 交付证明。
