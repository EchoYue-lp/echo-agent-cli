# ADR 0013：Subagent 控制回执投影

## 状态

已采纳，2026-08-27。

## 背景

TaskRun 内 Subagent 的 message、follow-up、interrupt 和终态需要同时支持精确 attempt、跨
重启恢复和多入口投影。直接暴露内部控制状态会让 GUI/TUI/CLI 建立不同的 reducer。

## 决策

以 `SubagentControlService` 作为 attempt-scoped authority，使用带 execution、attempt、
revision 和 generation 的 typed receipt。`AgentControlService` 只做 discriminator、bounded
query 和 surface 适配，不拥有新的 mailbox、TaskRun store 或 terminal reducer。

## 影响

所有 surface 获得一致的 message/follow-up/wait/interrupt 语义；迟到旧 attempt 不能污染新
revision，receipt 投影可以从 durable journal 重建。

## 2026-10-27：保留 TaskRun 身份与下一次尝试指导的归属

### 背景与候选

同一主 Agent 的 registry 会跨 TaskRun 复用。旧应用 helper 用无 Run 的
`SubagentAttemptIdentity::new` 投影已持久化的身份，导致不同 Run 的同名任务／attempt
碰撞已完成或仍活跃的记录；原下一次尝试指导又进入无 scope 队列，可能被另一 Run 消费。

清空完成记录、随机改 task ID 或新建每 Run registry 都会破坏既有去重、控制路由或共享
生命周期，因此不采用。框架已经提供 durable command identity 的 `attempt_identity()`
及同一原队列的 `queue_guidance_scoped`，应用只补原有适配接缝。

### 决策与数据流

应用从真实 Run／claim 构造一个 `SubagentControlIdentity`，再调用它已有的
`attempt_identity()`。Run 与 control scope 完整保留；尚未发出的命令继续使用空
`command_id`，不为投影虚构命令或 execution。只读、writer 文本与 writer 附件三条真实
消费者共用这个 helper。

下一次尝试指导仍先由原 TaskRuntime journal 完成 revision、命令幂等和目标校验。
转交时从目标的同一 attempt 投影取得 scope／Run，调用原框架 scoped queue；缺少身份
明确失败，不回退无 scope 队列。准入仍由原 registry 一次领取，活消息、interrupt、物理
结算和有界去重规则保持不变；旧无 scope API 继续供框架其它合理复用方使用。

### 验收与影响

同一真实 registry 对不同 Run 的相同 task／attempt 分别验证 active 与 settled 场景，
同 Run 重复及 execution 改绑继续拒绝。两份真实 journal 经原 transfer、queue 和实际
admission 只各消费自己的指导一次，并保留错 scope／旧 revision／重复 command、空身份
以及旧无 scope 的正反控制。readonly／writer 文本／writer 附件的真实 Fork 回归使用
应用 helper，避免绕开生产接缝。

本次不新增存储、任务状态或执行器，也不改变等待／栈／层数、SDK pin 或协议。既有示例
调用签名无需修改；框架与应用的适用示例编译、完整门禁、新原生及 main／CI 仍按各自
实际快照单独取得资格，定向控制回归不替代真实只读工厂的深度验收。
