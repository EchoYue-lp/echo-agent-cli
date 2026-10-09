---
schema_version: 1
id: rule.conversation-collaboration-authorities
kind: rule
status: verified
expectation: human_confirmed
risk: high
primary_focus: state_authority
focus: [data_durability, time_lifecycle, failure_concurrency, contract_evidence]
observed_at: 40fd80447cdf3d7627399af0b2069a8b4da5327a
behavior_refs: [behavior.side-conversation-lifecycle, behavior.gui-conversation-fork, behavior.automatic-agent-collaboration-ui-boundary]
code_refs: [echo-agent-app-core/src/state/config.rs, echo-agent-app-core/src/agent_router/router.rs, echo-agent-app-core/src/conversation_archive.rs, echo-agent-app-core/src/managed_conversation.rs, echo-agent-app-core/src/conversation_input.rs, echo-agent-app-core/src/foreground_turn.rs, echo-agent-app-core/src/conversation_deletion.rs, echo-agent-app-core/src/state/app_state.rs, echo-agent-app-core/src/state/workspace.rs, src/tauri/commands/conversations.rs, src/tauri/commands/chat.rs]
evidence_refs: [evidence.side-conversation-preflight, evidence.gui-conversation-fork, evidence.automatic-agent-collaboration-ui-retirement]
finding_refs: [finding.manual-agent-collaboration-ui-retirement]
---

# Conversation collaboration authorities

## 不变量或唯一权威

ConversationStore 是 transcript 权威，AgentRouter 是 Conversation inbox/group 权威，ConversationInputService 是 durable input frontier，ForegroundTurnControl 是活跃 turn admission/cancel 权威，ChatEventLog 是普通对话事件权威；EKO conversation archive 只持有界面可见性与跨工作区交接事实，不改写 transcript。

## 适用行为

适用于普通会话和 Side Conversation 的创建、快照、消息、模型配置投影、取消、恢复、重命名、删除与内部 Agent 通信。

## 当前实现

EKO 已通过 file-backed ConversationStore、AgentRouter JSON、conversation-scoped AgentPool、durable input facts、foreground lease 和 deletion service 分别实现这些职责。

## 期望行为

Agent 使用既有模型工具和运行时自动协作；GUI 不提供人工消息、组管理或协作动态控制台。用户参与并行探索使用 Sidechat/Forkchat，任务/HITL 的既有结果与控制呈现不因此移除。

Side Conversation 只组合现有权威；Tauri/GUI adapter 不拥有复制器、父子 store、队列、取消状态机、事件 reducer 或 executor，非 GUI surface 不接入专用合同。公开 AgentGroup 查询和修改入口不暴露 Side group，只有 crate-private Side service 可访问其 raw record，并通过 AgentRouter 文件锁内 mutation 更新最新记录的目标字段。

## 证据

app-core 与 Tauri/GUI 的真实调用路径、GUI branch 的编辑/重新生成与完整回合 Fork、既有 TUI fork、Agent control ADR 和 Side Conversation Design 提供边界依据。

## 裁决记录

分层决策将 Side Conversation 留在 EKO 应用层，`echo-agent` 通用 framework 不增加产品字段或状态。
