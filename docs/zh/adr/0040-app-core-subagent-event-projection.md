# ADR 0040：App-Core Subagent 事件投影

- Status: Accepted
- Date: 2026-09-05
- Owners: `echo-agent-app-core`、`src/tauri`、`web-frontend`

## 背景

ADR 0039 要求 EKO 把一个 Subagent attempt 呈现为有序的 Agent 时间线。framework
现已提供版本化 `SubagentEventEnvelope`，包含稳定身份、attempt 内 sequence、parent
关联、有界 replay、gap 报告和保留终态对账。

旧 EKO 路径只在 Tauri 订阅 bootstrap Agent 的 raw `SubagentEventBus`。它会漏掉 pool 和
workspace Agent，依赖 execution-id 字符串重建 Task 身份，在桌面 adapter 内生成 usage
序号并缓存 ordinary chat 地址，丢弃 thinking/token event，broadcast lag 时也只写日志。
TUI 还维护了另一条 raw 订阅。因此旧路径无法形成唯一、持久、surface-neutral 的合同。

## 候选方案

1. 保留各 surface 的 raw 订阅，继续增加 Tauri/TUI cache。
2. 新增专用 Subagent conversation store 和 reducer。
3. 让 EKO 的所有 Agent generation 共用一个 framework event bus，并由 app-core 只投影
   一次，复用现有 `ChatEventLog` 与 `ToolExecutionProjector`。

## 决策

采用方案 3。

- bootstrap、pooled conversation 和 workspace Agent 注入同一个 `SubagentEventBus` 的
  clone；app-core 只有一个 service 订阅权威 envelope stream。
- EKO 进程级 Subagent admission 只在 Agent 构造期或 task tool 注册期安装；`execute_run`
  不再修改 Agent。`task_execute` 可能运行在同一 Agent 的外层 ReAct write lease 内，运行时
  再取写锁会在 dispatch 前形成自锁。
- `SubagentEnvelopeProjector` 校验 framework identity/content hash，不读取当前 UI focus
  即可定位 TaskRun owner；只有没有 run 的 ordinary dispatch 才使用 exact foreground turn
  fallback。
- EKO 禁止解析 execution-id 字符串。framework 的 task、attempt、revision、lineage、
  stream、event、hash、sequence、timestamp 和 parent id 全部保留在 ts-rs 生成的
  `SubagentEventMetadata`/`ExecEvent` 中。
- projector 只向既有 `ChatEventLog` 追加一次
  `ChatDriverEvent::Execution(ExecEvent)`。工具详情仍由既有 `ToolExecutionRepository`
  派生，不新增持久化 store。工具投影失败时立即重试，并作为有界的进程内 debt 留给后续
  event 继续偿还；boot recovery 仍可从权威 ChatEventLog 重建，且不会再次追加 execution event。
- framework sequence 跳跃时先调用 `replay_after`。若有界 retention 仍无法返回连续
  suffix，EKO 提交 typed `subagent_stream_gap`，同时继续对账保留的 terminal。lag recovery
  扫描 retained、active 与已知 stream 的并集；即使某个 active stream 的完整 replay suffix
  被其它 stream 挤出，publisher 仍保留不可变 dispatch-start 身份作为地址锚点。
- `ChatEventLog` 只保存 active turn 的 weak、临时 live-sink registration，并在释放
  registry lock 后才执行 callback。没有 live turn 的已提交后台事件通过 app-core committed
  projection stream 交给长生命周期的 GUI、TUI 和 REPL adapter。该 stream 提供有界 replay
  snapshot；subscriber 晚绑定或 broadcast lag 后按 Chat event id 去重恢复。one-shot JSONL
  在响应关闭后没有 unsolicited output owner；请求级 channel 在下一次响应中从会话 durable
  cursor 重放已提交 execution event。
- Tauri 只发布已提交的 chat envelope 和二级 tool summary，不再订阅或解释 raw framework
  Subagent event。active TUI、CLI 和 channel turn 通过原有 bound chat sink 接收同一
  `ChatDriverEvent::Execution`；TUI 与 REPL 在 turn-local sink 关闭后还会消费 committed
  fallback。
- 前端只导入 ts-rs 生成的 `ExecEvent`；live `chat://event` 和
  `replay_chat_events` 进入同一个 handler 与 Subagent reducer，并按 framework event
  id/sequence 幂等应用。
- delegated read/write PlanTask 不再重复发送 application start/terminal trace；TaskRuntime
  的 `SubagentAssigned`/`SubagentReleased` 仍是 durable task lifecycle 权威。primary-direct
  task 没有 framework Subagent envelope，因此继续保留 app-owned trace。
- 应用关闭时 projector 持续运行，等已接受 TaskRun、workspace pool 和 primary Agent
  结算后，再 drain 并 join。

## 影响

- reload 恢复的 typed execution event 与 live 收到的合同相同。
- thinking、token、tool、usage、uplink、isolation 和 terminal 不再被桌面 adapter 静默
  丢弃。
- transient range 缺失会明确显示，但不否定保留的 durable boundary 和 terminal output。
- synthetic gap 的 ChatEventLog 顺序与 framework stream 顺序分域，journal sequence 不会压掉
  后续 framework terminal。
- surface adapter 不再拥有事件身份与顺序策略；app-core 是唯一 EKO 投影实现。
- `SDK-Docs-Impact`: none；本决策只消费 framework ADR 0030 已记录的公共 API，改变的是
  EKO 产品集成。
- `SDK-Skill-Impact`: none；Skill discovery/execution 合同不变。
- `Website-Impact`: none；官网不发布 EKO runtime 内部合同或本应用界面，因此不修改官网内容与
  generated index。
