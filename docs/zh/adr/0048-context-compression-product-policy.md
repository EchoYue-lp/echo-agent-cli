# ADR 0048: EKO 上下文压缩产品策略

## Status

accepted

## 背景与候选

框架已提供 canonical/protected context、按 token 选择近期 turn 的压缩器和带 focus/cancel
的手动压缩入口。EKO 已有 TaskRuntimeStore、goal/recovery projection 与 app-core
manual_compression。永久保留所有用户输入会破坏活动窗口的上界；另建 goal store 或在
应用复制 selector 会形成平行权威，因此复用现有机制。

参考框架 ADR 0081 中 Codex 的有界用户材料和重建初始上下文，以及 Pi 的近期 token
预算与工具组边界。EKO 本地个人助理的特有策略是保存原始 goal、用户 steer 和产品 journal，
这些能力保留在应用层。

## 决策

- EKO 默认 `compress_window: 0`，使用模型窗口 25%、至多 20K token 的近期原文预算。
  正数显式选择旧消息条数上限；`adaptive` 仍使用自身的 L1-L5 配置。
- `/compress`、`/compact`、TUI 与 GUI 使用同一已安装策略，不再宣称固定 6/12 条覆盖。
  各入口传入同一个可选 focus；GUI 提供压缩重点输入框，TUI 接受命令后的文本。
- app-core 持有 foreground/AgentPool/workspace I/O owner，取消传给框架并等待结果。
  变换前取消保留上下文；变换成功后即使 caller drop/cancel，journal safe point 仍完成。
- Goal contract 与 recovery capsule 来自唯一的 TaskRuntimeStore。最近四个已记录 user steer
  按时间顺序保留其完整有界 excerpt，不再在 projection 上二次截成 200 字符。
  它们不是新请求或新的 canonical system policy，退出 run scope 时删除 projection。
- ConversationStore 保存完整 transcript，ContextManager 保存活动窗口，RuntimeStateStore
  保存恢复 checkpoint，TaskRuntimeStore 保存 goal/执行事实，Trace/ChatEventLog 保存诊断和事件。

## 影响与回滚

删除 app-core request 和前端中未生效的 `keep_messages` 参数。开发阶段不保留这一旧接口。
框架 Rust 构造器保留，EKO 正数窗口继续支持显式旧策略。回滚本切片恢复旧配置和 adapter，
不会删除会话历史或 TaskRuntime journal。框架先交付 main 后，应用才能合并 main。

## 验收

四类 surface focus 都产生一份 checkpoint 和一条 journal 事件；取消、caller drop 和 journal
失败保留原结算合同。真实压缩后 goal 和 steer 的末尾限制仍在，重复刷新不累积 projection。
Rust、GUI 与前端验证分别报告，不能把 mock 测试当作原生窗口或外部模型验收。
