# ADR 0016：统一模型 Agent 协作控制面

## 背景

Iteration 3 需要让模型直接发现、检查、消息协作、等待和中断 Agent，同时保持
Conversation Agent 与 TaskRun 内 Subagent 的语义边界。仓库已经有 `AgentRouter` 的
durable inbox、`SubagentControlService` 的 exact-attempt guidance/interrupt、以及
`TaskRuntimeStore` 的事件 journal；再建 mailbox、store、执行器或状态 reducer 会制造
第二事实源。

参考依据包括本仓库已固定的一手实现对照：Claude Code 的 subagent/消息工具采用显式
目标和 bounded 结果，OpenAI Codex app-server/exec 采用 Thread/Turn/Item 生命周期、事件
游标和 wait/interrupt 控制；本阶段只采纳这些跨实现稳定语义，不依赖未公开实现细节。

## 决策

在 `echo-agent-app-core::agent_control` 增加一个薄 application adapter：

- `ConversationTarget { workspace_id, conversation_id, workspace_generation? }` 只路由
  `AgentRouter`；
- `TaskSubagentTarget { workspace_id, run_id, task_id, plan_revision, execution_id,
  attempt, workspace_generation? }` 只路由 workspace-scoped `SubagentControlService`；
- 注册 `agent_list`、`agent_inspect`、`agent_message`、`agent_followup`、`agent_wait`、
  `agent_interrupt` 六个模型工具；既有 `agent_tool` 继续作为一次有界 dispatch，不新增
  `agent_spawn`；
- message/follow-up/interrupt 在进入 owner 前验证 discriminator、revision、attempt 和
  workspace incarnation；不匹配时返回 typed fail-closed error；
- `agent_wait` 只读取现有 router/task event cursor，返回 bounded event summaries，不能
  代替 TaskRun/Subagent terminal authority；取消和超时是 wait 结果，不是任务终态；
- Conversation `agent_message`/`agent_followup` 先通过绑定 workspace 的
  `ConversationStore` 验证持久化会话，再以 `message_id` 复用 router exact-once 语义；router
  的 `target.json` 单独不能制造会话。`agent_message` 只持久化信息、不自动启动 turn，
  `agent_followup` 才复用 AppState delivery supervisor wake。TaskSubagent command 以
  `command_id` 复用其 workspace `SubagentControlService` durable receipt，并在 duplicate
  replay 前校验 command kind/content；
- 工具通过 `AppState::register_agent_control_tools` 在共享 ToolManager 上注册，故
  GUI/TUI/CLI/channel、global pool 与 workspace pool 共用同一套 schema 和
  router/registry authority；每个 pool 绑定自己的 TaskRuntimeStore，follow-up 复用 AppState
  的既有 delivery supervisor wake。每个注册的 `AgentControlService` 是单 workspace
  scope：global primary 与各 workspace pooled primary 分别绑定自己的 TaskRuntimeStore /
  ConversationStore；跨 workspace target 不在 service 内动态 retarget，而是 typed
  `target_unavailable` fail-closed，调用方必须先取得目标 workspace 的 scoped primary。

## 分层与取舍

通用框架能力仍留在 `echo-agent`（tracked steer、Subagent executor、Tool trait）；EKO
产品策略与 target discriminator 留在 application；adapter 只转换类型、注入 metadata、
调用既有 service。没有新增 task graph mutation API，因此模型仍必须通过
`task_update(base_revision)` 修改正式图。

## 影响

模型获得统一且有界的协作控制面；surface 不再需要各自解析 Agent 地址或实现第二套
list/message/wait。Conversation 的存在性由绑定 workspace `ConversationStore` 判定，Router
只负责 inbox/journal；list 会过滤 router-only phantom target，并为每个列出的 target
写入当前 workspace generation（global 使用固定 `global`）。WorkspaceRegistry generation
读取通过 blocking boundary 完成。TaskRuntime 的所有读操作经
`TaskRuntimeBlockingAdapter` 离开 async executor；当前底层 `list_events`/
`list_subagent_runs` API 仍返回完整向量，adapter 保证 exact-target 过滤发生在
`MAX_EVENTS` 截断之前，避免 false timeout；新增真正的 bounded query API 留作 R1/P0
integration，不能在本 ADR 中伪称已解决。

## 验证

覆盖 target discriminator、ConversationStore-first phantom rejection、exact-once message、
cursor target binding、settled attempt inspect/wait、cancel/timeout wait，以及六工具
schema/注册；提交前按 `AGENTS.md` 执行 Rust workspace 门禁。
