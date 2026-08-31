# ADR 0007: TaskRuntime Agent Turn 权威

状态：已采纳

## 背景

TaskRuntime 曾有两处应用层 raw Agent stream loop：PlanTask 回落到 primary Agent 的执行，
以及 background/cron/`create_complex_task` 独立 run 的 planning turn。它们自行推断 terminal、
累计 usage、压平 failure，并与 chat continuation 分别结算 RunTurn，造成 missing terminal 被当作
成功、provider retry/timeout 丢类型、Goal Completed 与 active RunTurn 并存等问题。

## 候选方案

1. 保留两处 loop，仅抽取事件 match helper。改动小，但 stream、terminal 和 retry 仍有多个权威。
2. 把 EKO TaskRuntime、journal、HITL 和 UI 投影下沉 framework。能统一实现，但会用产品策略污染
   通用框架。
3. framework 拥有有限 Agent turn；EKO 保留一个 sink 和一个 RunTurn lifecycle service。本项目
   采用此方案。

## 决策

- `AgentTurnDriver` 独占 raw stream 启动、envelope sequence、exact terminal、typed failure、
  cancellation 和 provider-reported receipt。
- `EkoAgentTurnSink` 是 TaskRuntime 唯一 adapter，负责 ExecEvent、event-id usage、tool boundary、
  evidence 和 artifact。未报告的 usage 不计入 budget。
- `turn_lifecycle.rs` 是 EKO 唯一 RunTurn terminal service。chat、owned background 和 pre-driver
  rejection 都通过 `TaskRuntimeOperation` 调用它。
- Owned driver 复用 continuation eligibility、durable provider deadline、cancel/shutdown 和 cell wake，
  不注册 detached owner，也不维护第二套 active/generation/pending 状态。
- PlanTask terminal UI 事件只在 exact claim CAS 提交后发布。Physical Subagent attempt terminal 与
  PlanTask terminal 明确分离。
- Active RunTurn 内的 Goal Completed 与 `RunTurnFinished` 同一 journal batch 提交。Task graph 或
  direct summary 先达到 quiescent，不能单独把 Run 改为 Completed。
- Independent planning Agent 在 invocation 构造前从完整 tool registry 派生所有 Write/Execute
  capability 并加入 `disabled_tools`，包括动态 plugin/MCP tool；sink 仍审计真实 tool identity。
  即使模型尝试 mutation tool，producer handler 也不会执行，direct completion 同时 fail closed，
  必须使用正式 writer PlanTask。动态 Read tool 不受影响。
- Attended run 携带 surface HITL provider；Unattended run 不等待交互 owner。

## 影响

- 删除 `AgentDriveStreamResult` 和两处 raw stream loop，不再从“stream 返回”推断成功。
- Typed provider failure 进入 durable RunTurn/Subagent evidence；retryable LLM failure 使用稳定
  fingerprint requeue，最终 timeout 保持 `TimedOut`。
- Direct completion 可保留只读 evidence、verification 和 artifact，但拒绝 mutation evidence。
- Framework trace 没有 Paused 变体，因此 Paused 不写可选 trace，绝不投影为 Completed。

## 业界依据

- Codex 非交互事件流以显式 completed/failed terminal 表达 item 生命周期，不以 EOF 代替终态。
- Temporal 以 event history 和 durable retry policy 恢复执行，并把业务状态与 retry 分开。
- LangGraph 以 checkpoint/thread identity 恢复 interrupted execution。

这些实现共同支持：有限执行必须有显式 terminal；retry、恢复和产品投影消费 typed durable fact，
不能各自解释原始流。

## 不适用范围

本决策不改变 framework public API、examples 或 website 展示内容；它只规定 EKO 如何消费已有
`AgentTurnDriver`，以及 TaskRuntime 本地文件权威如何原子结算。
