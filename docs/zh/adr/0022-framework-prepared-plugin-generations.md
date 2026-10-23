# ADR 0022：Framework-prepared Plugin Generations

## 状态

已采纳并实现，2026-08-29。

## 背景

Plugin preparation、workspace target、primary Agent、pool Agent 和 future Agent 的发布
曾可能各自刷新，导致 generation 不一致或旧 target 迟到覆盖新 target。

## 决策

framework 负责从根 `plugin.json` 解析、校验和生成不可变 prepared generation。EKO 在一次
捕获的 workspace target 上执行产品组件 publication，并使用 apply receipt 完成原子 fanout
和 rollback。所有现有/未来 Agent 都绑定同一 generation。

## 影响

Plugin 和 Skill/monitor/theme 等产品投影不再重复 preparation；cold workspace 继承 committed
generation，旧 generation 的迟到结果按 identity fail closed。

## 发布失败后的清理归属（2026-10-23）

清理收据保留原目标权威：Agent handle、框架 wiring receipt、MCP 名称 registry，以及
完成原操作所需的 scheduler 或 lifecycle manager。项目路径只用于检索，不能授权在新
Agent 或新 registry 上重放。不同 registry 发放的数值 token 不能相互替代。

bootstrap 失败和 runtime 退休沿现有应用重试路径保留原清理权威。停放不能丢弃 wiring，
也不能将 Agent 或 transport 的 Drop 当作物理清理完成。接管未结算时必须在新代发布前
拒绝 bootstrap；partial restore 和 pool rollback 保留同一精确收据，并明确失败，不能
把恢复不完整的 generation 报为 live。

shutdown 由独立持有资源的结算任务执行。调用者只等待收据，取消等待者不能销毁待撤回
的 wiring 或名称 token。这里扩展既有 supervisor，不建立第二套插件发布权威。

MCP 配置的操作准入与 generation 失效共享有序边界。后台连接取得 Agent 写锁后，在
启动框架 preparation 前复核原 generation。新操作确认的 unchanged client 不能再被旧
操作撤回；preparation 一旦开始，原任务必须持续持有它直到清理完成。

参考 Tokio 官方的 [graceful shutdown](https://tokio.rs/tokio/topics/shutdown) 与
[cancellation safety](https://docs.rs/tokio/latest/tokio/macro.select.html#cancellation-safety)：
发出信号或丢弃观察者不等于 join 原资源 owner。EKO 复用已有 supervisor 和框架精确
publication receipts，不新增框架 API。
