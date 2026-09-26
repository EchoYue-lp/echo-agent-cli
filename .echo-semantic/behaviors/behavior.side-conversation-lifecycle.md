---
schema_version: 1
id: behavior.side-conversation-lifecycle
kind: behavior
status: verified
expectation: human_confirmed
risk: high
primary_focus: time_lifecycle
focus: [state_authority, data_durability, failure_concurrency, contract_evidence]
boundary: boundary.eko-conversation-collaboration
observed_at: source:caa0bf81fcda8d046037a8a1065adc31b7bb0652059b4e4c8e975d8831e26fc0
code_refs: [AGENTS.md, README.md, docs/supreme/specs/side-conversation/design.md, echo-agent-app-core/src/lib.rs, echo-agent-app-core/src/api/mod.rs, echo-agent-app-core/src/side_conversation.rs, echo-agent-app-core/src/conversation_archive.rs, echo-agent-app-core/src/managed_conversation.rs, echo-agent-app-core/src/agent_router/address.rs, echo-agent-app-core/src/agent_router/recovery.rs, echo-agent-app-core/src/agent_router/router.rs, echo-agent-app-core/src/agent_pool/pool.rs, echo-agent-app-core/src/state/app_state.rs, echo-agent-app-core/src/state/workspace.rs, echo-agent-app-core/src/state/tests.rs, src/tauri/commands/conversations.rs, src/tauri/commands/chat.rs, web-frontend/src/api/endpoints.ts, web-frontend/src/api/endpoints.sideConversation.test.ts, web-frontend/src/stores/conversationStore.ts, web-frontend/src/components/layout/LeftSidebar.tsx, web-frontend/src/components/chat/ChatPanel.tsx, web-frontend/src/components/chat/MessageBubble.tsx, web-frontend/src/components/chat/MessageBubble.completed.test.tsx, web-frontend/src/components/chat/SideConversationDialog.tsx, web-frontend/src/generated/AgentGroup.ts, web-frontend/src/generated/SideConversationCreateReceipt.ts, web-frontend/src/generated/SideConversationCreateRequest.ts, web-frontend/src/generated/SideConversationEntry.ts, web-frontend/src/generated/SideConversationGroupMetadata.ts, web-frontend/src/generated/SideConversationStatus.ts, web-frontend/src/generated/TurnVisibility.ts, echo-agent-app-core/src/agent_control.rs, echo-agent-app-core/src/conversation_input.rs, echo-agent-app-core/src/conversation_deletion.rs]
rule_refs: [rule.conversation-collaboration-authorities]
evidence_refs: [evidence.side-conversation-preflight]
finding_refs: []
---

# Side Conversation lifecycle

## 重要承诺

主对话可以创建多个携带 committed transcript 快照的一级 Side Conversation；每个支线独立交互和取消，内部消息不自动污染主 transcript，重启后关系与内容可恢复。

## 当前行为

候选实现已在 app-core 组合 ConversationStore、AgentRouter、AgentPool、input/turn/event/deletion 权威，并由 Tauri/GUI 投影 Side Conversation。GUI branch 与 TUI `/fork` 继续保持各自普通分叉语义，非 GUI 没有 Side Conversation 专用合同。

## 期望行为

GUI/Tauri 通过一个 app-core authority 创建、列出、打开、发送、取消、更新、重命名和删除一级支线；主对话保持独立可用，完成结果不自动回写。TUI、CLI/JSONL 与 channel 保持既有行为。

## 触发、结果与副作用

GUI 用户触发创建，用户或主 Agent 触发内部消息；结果包括新 conversation、committed snapshot、AgentGroup member、first-turn receipt、事件和可恢复 GUI 投影。

## 失败、重试与恢复

创建必须幂等；快照要求 transcript revision、内部消息 ID 和 pending intent 前后一致，未结算时不发布半成品关系。首轮通过完整 GUI chat driver 执行且失败时保留可重试 child；稳定 prompt 在慢首轮期间可见，relation create 到 foreground admission 始终持有 identity guard；取消按 exact turn，删除与创建按 identity guard 串行化并按 child/parent 范围收敛，重启从既有文件权威重建。

## 证据

定向与全量测试、GUI 条件矩阵、前端测试/构建、Playwright 布局、非 GUI 零差异检查、严格语义校验和独立 Review 共同证明 Side Conversation service、完整 GUI 首轮驱动、快速完成/响应丢失后的稳定 prompt identity、父子投影、列表/加载隔离、锁内原子 metadata mutation、持久 viewed marker、模型隔离、删除竞态收敛和文件重启恢复闭合。

## 裁决记录

用户确认 Side Conversation 是 GUI 专属的一级可见 Subagent 布局能力，禁止嵌套、SQLite、新表、第三聊天运行时、自动主 transcript 回写和非 GUI 专用入口；这不削减其它 surface 的核心 Agent 能力。
