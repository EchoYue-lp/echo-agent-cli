# EKO 运行中补充输入与打断设计

日期: 2026-07-11

## 结论

运行中的新输入不能并发启动第二个 foreground turn。它只能走两条路径:

1. 当前 turn 支持 steer 时,注入当前 turn,在下一个 Agent 安全点生效。
2. 当前 turn 不支持 steer 时,进入可见 FIFO,当前 turn 终态后逐条启动新 turn。

打断是与补充输入独立的控制通道。`Esc` / Stop 必须触发当前 turn 的
`CancellationToken`,等待执行层发出终态后才能释放该 conversation 的执行权。

## 业界实现

### Claude Code

Anthropic 官方仓库的 changelog 明确记录:

- 从 0.2.75 起,Claude 工作时按 Enter 会排队附加消息。
- 后续修复覆盖 queued prompt 的图片、历史、编辑、扩展思考和 subagent 场景,说明
  queue 是正式的一等状态,不是临时 UI 文本。
- `Esc` / `Ctrl+C` 是真实 interrupt;有 queued prompt 时还会避免误取消并允许把消息
  拉回输入框编辑。

参考:

- https://github.com/anthropics/claude-code/blob/main/CHANGELOG.md#0275
- https://github.com/anthropics/claude-code/blob/main/CHANGELOG.md

### Codex

Codex 当前比单纯 FIFO 多一层 same-turn steering:

- app-server 暴露独立的 `turn/start`、`turn/steer`、`turn/interrupt`。
- `turn/steer` 带 `expectedTurnId`,防止客户端缓存的 active turn 已过期时把输入送错轮次。
- Regular turn 可 steer;Review / Compact 明确返回 not-steerable。
- TUI 对 steer 请求保留 pending/rejected 队列;不可 steer 时回退 follow-up FIFO。
- turn 完成后一次只发送一个 queued input;中断时会恢复或重新提交尚未确认的 steer,
  防止用户输入丢失。

参考:

- https://github.com/openai/codex/blob/main/codex-rs/docs/codex_mcp_interface.md
- https://github.com/openai/codex/blob/main/codex-rs/app-server-protocol/src/protocol/v2/turn.rs
- https://github.com/openai/codex/blob/main/codex-rs/core/src/session/mod.rs
- https://github.com/openai/codex/blob/main/codex-rs/tui/src/chatwidget/input_flow.rs

## EKO 现状

- 框架已有贯穿 LLM、tool batch、subagent 的 `CancellationToken`,具备真实取消基础。
- TUI 当前工作区改动已加入 `active_cancel` 与 `queued_turns`,采用 FIFO + cancel。
- GUI 之前在 streaming 时直接丢弃 Enter;后端也没有 foreground conversation busy
  约束,旁路调用可能并发污染同一 pooled agent。
- GUI 的 `InterruptPrompt` 面向持久 TaskRuntime run 的恢复/废弃,不等价于 foreground
  chat turn 的补充输入。
- framework ReAct loop 当前没有 pending-input mailbox,因此还不能安全实现 Codex 式
  same-turn steer。

## 分层

### echo-agent

放通用执行原语:

- `TurnInputMailbox` / `steer_input`。
- active turn id 预条件。
- 仅在 ReAct 安全点消费输入:LLM 响应结束后、tool batch 结束后、下一次 LLM 请求前。
- `SteerAccepted`, `NoActiveTurn`, `TurnMismatch`, `NotSteerable`, `EmptyInput` 结果。

这些能力与 EKO UI 无关,任何基于框架的交互式 agent 都可复用,属于框架层。

### echo-agent-cli

放产品交互与调度:

- 每 conversation 最多一个 foreground turn。
- pending steer 与 queued follow-up 的可见列表、取回编辑、删除、附件显示。
- TUI / GUI / channel 的快捷键、按钮和渲染。
- steer 失败自动降级 FIFO。

这些依赖 EKO 的会话/UI 决策,属于应用层。

## 交付阶段

### Phase 1:立即可用

- TUI 与 GUI 运行中 Enter 进入 FIFO。
- 当前轮终态后只派发一个队首消息。
- Stop 触发真实 cancel,不并发启动下一轮。
- 后端按 conversation 加 foreground busy guard,防止 UI 旁路并发。
- 队列数量可见;清会话、重新生成、编辑重发会清理旧队列。

### Phase 2:Codex 式 steer（已完成）

- `echo-agent` 已增加 active turn lease、mailbox、turn id 前置条件和结构化错误。
- ReAct 在模型调用前、工具批次后和最终文本提交前消费 steer。
- GUI 普通 Enter 保留为可排序 FIFO;每个队列项可手动“补充到当前任务”。
- steer 不可用时队列项原地保留,不丢失、不弹阻塞式选择框。
- GUI 队列支持鼠标拖拽排序和删除;TUI 保持 FIFO,并提供 `/steer <指令>`。
- foreground run 的 terminal 状态会先持久化并释放 conversation ownership,再发 Done。

## 不变量

1. 同一 conversation 绝不同时运行两个 foreground turn。
2. 用户已提交的输入只能是 pending steer、queued follow-up、committed user message 三者之一。
3. cancel 不等于丢弃队列。
4. 只有收到执行层终态才能释放 active turn。
5. 队列按提交时冻结附件和 interaction mode。
6. 所有字符串预览使用 UTF-8 安全的字符迭代。
