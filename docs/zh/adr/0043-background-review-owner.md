# ADR 0043：EKO 拥有 Background Review 的准入与结算

## 状态

已采纳

## 背景

框架现在返回带稳定 `ReviewIdentity` 的惰性 `BackgroundReviewHandle`。可选的 Memory 写入可能先于调用方观察到 `ReviewOutcome` 完成；如果此时调用方取消或进程退出，应用就没有终态收据。框架 [ADR 0058](https://github.com/EchoYue-lp/echo-agent/blob/main/docs/adr/0058-background-review-settlement-ownership.md) 把准入、关闭和结果结算交给嵌入应用；[ADR 0065](https://github.com/EchoYue-lp/echo-agent/blob/main/docs/adr/0065-evolution-memory-audit-reconciliation.md) 提供 Memory journal 恢复和精确 key 查询，但不定义 EKO 的 Review 生命周期。

Tokio 的[优雅关闭说明](https://tokio.rs/tokio/topics/shutdown)区分取消信号和等待任务结束；[`JoinHandle::abort` 契约](https://docs.rs/tokio/latest/tokio/task/struct.JoinHandle.html#method.abort)也需要 await handle 才能观察取消。EKO 已拥有 generation lease 和 Review Inbox，因此生命周期归 app-core，而不是通用框架或各个界面适配器。

## 候选方案

1. GUI、TUI、CLI 各自 spawn Review。拒绝：观察者丢失和进程退出时没有共享结算与恢复权威。
2. 把 EKO 收据和 Review Inbox 策略放进 `echo-agent`。拒绝：workspace generation、关闭和 UI 证据是应用特有语义。
3. 由 app-core owner 准入惰性 handle，并记录持久收据 journal。采用。

## 决策

1. GUI、TUI、CLI 保留现有普通 Review 交互，但都把框架惰性 handle 交给 `ReviewGenerationLease::track_background_review`。Side Conversation 仍是 GUI 独有布局能力，与这项核心 Review 能力无关。
2. owner 先记录稳定 Memory key 在准入前的指纹，再在首次 poll 前持久确认 `Admitted(operation_id, ReviewIdentity, authority_scope, workspace_generation, memory_before)`。同一 run 的活跃 Review 重复准入会被拒绝；关闭准入后绝不 poll handle。恢复时拒绝属于其它 workspace generation 的未结算收据。
3. supervisor 持有 generation lease，先记录完整 `ReviewOutcome`，再把有证据支持的候选投影到现有 Review Inbox，最后写终态收据。Inbox upsert 幂等，因此可以恢复 outcome 与 Inbox 写入之间的崩溃。
4. 调用方退出或应用关闭时，owner abort 并 await 内层任务，然后恢复框架 Memory journal、比较稳定 persistence key 与准入前指纹。收据区分新观察到的写入、没有写入和旧值存在时无法归因；不得把前一次 Review 的 Memory 归给本次中断操作，也不能凭空补造引文和候选。
5. 启动时在发布主 Agent 前重放未完成准入：有 durable outcome 则补投影并结算；只有准入则完成 Memory 恢复并标记中断。恢复失败阻止 bootstrap，不能静默丢弃债务。
   owner 按 operation ID 提供只读 typed receipt，GUI Review 响应携带该 ID 以供后续查询。
6. 收据 journal 是 EKO 的 `.eko/evolution/background-review.jsonl` 文件状态，不是第二套 Memory 或 Review Inbox 权威。Review 分析、置信策略和 Memory 事务语义仍归框架。

## 影响

- 进程退出后可能存在已提交 Memory、却无法重构原 Review 证据的情况。终态收据明确暴露这种不确定性，可由用户显式重新 Review；不谎称原 Review 成功。
- 同次框架升级要求 Skill 生命周期改由框架 `SkillMutationAuthority` 和 EKO 独立业务 ChangeLog 处理；这不把 Background Review 变成 Skill mutation。
- 框架 examples 和 `echo-website` 不消费 EKO 应用层收据 API，无需同步修改。
