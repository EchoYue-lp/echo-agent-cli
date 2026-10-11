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
7. 桌面退出使用 Tauri 的 [`App::run_return`](https://docs.rs/tauri/2.11.2/tauri/struct.App.html#method.run_return)，再等待现有 `ApplicationServices` 结算完成，桌面入口才返回。`Builder::run` 调用会结束进程的 `App::run`，正常退出时无法执行放在它后面的异步清理。启动失败和非零退出码也必须结算同一个 owner，并在其收据中保留失败。
8. bootstrap Memory 指向已有 workspace 的 `.eko` 时，恢复使用该 workspace 的持久 scope 与 generation。主 Agent pool 与 workspace execution host 复用匹配的 ReviewIntegration 和 Store；不得以 `global/global` 恢复 scoped 债务，或为同一 binding 新建第二个准入 owner。已退休的 owner 保持关闭，真正属于其它 generation 的债务仍然拒绝恢复。
9. Memory 归因使用带版本的事实指纹，不包含召回计数和时间。已有旧格式的完整元数据指纹不能确认本次 Review 写入，恢复时报告归因未知。bootstrap 回滚先结算 command-cell 服务持有的 ProductData flow，再等待 ProductData 聚合关闭。
10. 复用 owner 时必须保留全部存活主 Agent 和 pool 的规则发布。目标采用去重弱引用，退休 workspace 不会被投影强引用保活，后绑定的 workspace 也不能替换 bootstrap 目标。发布在修改 Agent 指令前准备好全部 pool。关联项目未变化时不退休 Host，也不关闭其共享 Review 准入。
11. 每个投影目标保留自己解析后的项目根。bootstrap 显式 `--project Q` 对该 primary 和 seed pool 保持权威，workspace Agent 则使用持久关联项目 P。两者各自的项目指令都叠加同一个 workspace 的 learned rules 和 Memory 状态。发布前严格读取各目标快照；因项目输入不同，目标的 revision 可以不同。

## 影响

- 进程退出后可能存在已提交 Memory、却无法重构原 Review 证据的情况。终态收据明确暴露这种不确定性，可由用户显式重新 Review；不谎称原 Review 成功。
- 同次框架升级要求 Skill 生命周期改由框架 `SkillMutationAuthority` 和 EKO 独立业务 ChangeLog 处理；这不把 Background Review 变成 Skill mutation。
- 框架 examples 和 `echo-website` 不消费 EKO 应用层收据 API，无需同步修改。

## 验证

Review 回归经 app-core 准入驱动真实惰性 `BackgroundReviewHandle`，在持久 Memory 写入后阻塞 observer。调用方丢弃和 shutdown 都必须 join 子任务，暴露已提交 Memory 效果的 interrupted 收据，并且不能伪造 Inbox 候选。bootstrap 回归用持久 workspace 身份重开 scoped 债务并检查 owner 复用。桌面退出回归通过与真实入口共用的返回式事件循环 adapter 执行真实应用生命周期 owner；不宣称自动操作 OS 窗口或覆盖强制终止进程。
