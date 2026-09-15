---
schema_version: 1
id: rule.conversation-collaboration-authorities
kind: rule
status: verified
expectation: human_confirmed
risk: high
primary_focus: state_authority
focus: [data_durability, time_lifecycle, failure_concurrency, contract_evidence]
observed_at: source:87791a9dc0b2bb4484fff726fea064bd943e080f4f0e97b74d9d5821442c1e9a
behavior_refs: [behavior.side-conversation-lifecycle]
code_refs: [echo-agent-app-core/src/state/config.rs, echo-agent-app-core/src/agent_router/router.rs, echo-agent-app-core/src/conversation_input.rs, echo-agent-app-core/src/foreground_turn.rs, echo-agent-app-core/src/conversation_deletion.rs, src/tauri/commands/conversations.rs, src/tauri/commands/chat.rs]
evidence_refs: [evidence.side-conversation-preflight]
finding_refs: []
---

# Conversation collaboration authorities

## 不变量或唯一权威

ConversationStore 是 transcript 权威，AgentRouter 是 Conversation inbox/group 权威，ConversationInputService 是 durable input frontier，ForegroundTurnControl 是活跃 turn admission/cancel 权威，ChatEventLog 是普通对话事件权威。

## 适用行为

适用于普通会话和 Side Conversation 的创建、快照、消息、模型配置投影、取消、恢复、重命名、删除与内部 Agent 通信。

## 当前实现

EKO 已通过 file-backed ConversationStore、AgentRouter JSON、conversation-scoped AgentPool、durable input facts、foreground lease 和 deletion service 分别实现这些职责。

## 期望行为

Side Conversation 只组合现有权威；Tauri/GUI adapter 不拥有复制器、父子 store、队列、取消状态机、事件 reducer 或 executor，非 GUI surface 不接入专用合同。公开 AgentGroup 查询和修改入口不暴露 Side group，只有 crate-private Side service 可访问其 raw record，并通过 AgentRouter 文件锁内 mutation 更新最新记录的目标字段。

## 证据

app-core 与 Tauri/GUI 的真实调用路径、保持不变的 GUI branch 与 TUI fork、Agent control ADR、Side Conversation Design 和 GUI-only Plan 提供边界依据。

## 裁决记录

分层决策将 Side Conversation 留在 EKO 应用层，`echo-agent` 通用 framework 不增加产品字段或状态。
