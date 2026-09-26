---
schema_version: 1
id: asset.eko-conversation-authorities
kind: asset
title: EKO Conversation collaboration authorities
asset_type: state_authority
status: active
risk: high
observed_at: source:caa0bf81fcda8d046037a8a1065adc31b7bb0652059b4e4c8e975d8831e26fc0
boundary_refs: [boundary.eko-conversation-collaboration]
code_refs: [echo-agent-app-core/src/lib.rs, echo-agent-app-core/src/api/mod.rs, echo-agent-app-core/src/state/config.rs, echo-agent-app-core/src/agent_router/address.rs, echo-agent-app-core/src/agent_router/recovery.rs, echo-agent-app-core/src/agent_router/router.rs, echo-agent-app-core/src/agent_pool/pool.rs, echo-agent-app-core/src/conversation_archive.rs, echo-agent-app-core/src/managed_conversation.rs, echo-agent-app-core/src/conversation_input.rs, echo-agent-app-core/src/foreground_turn.rs, echo-agent-app-core/src/conversation_deletion.rs, echo-agent-app-core/src/side_conversation.rs, echo-agent-app-core/src/state/app_state.rs, echo-agent-app-core/src/state/workspace.rs, echo-agent-app-core/src/state/tests.rs, src/tauri/commands/conversations.rs, src/tauri/commands/chat.rs]
consumer_refs: [README.md, src/tauri/commands/conversations.rs, src/tauri/commands/chat.rs, web-frontend/src/api/endpoints.ts, web-frontend/src/api/endpoints.sideConversation.test.ts, web-frontend/src/stores/conversationStore.ts, web-frontend/src/components/layout/LeftSidebar.tsx, web-frontend/src/components/chat/ChatPanel.tsx, web-frontend/src/components/chat/MessageBubble.tsx, web-frontend/src/components/chat/SideConversationDialog.tsx, web-frontend/src/generated/AgentGroup.ts, web-frontend/src/generated/SideConversationCreateReceipt.ts, web-frontend/src/generated/SideConversationCreateRequest.ts, web-frontend/src/generated/SideConversationEntry.ts, web-frontend/src/generated/SideConversationGroupMetadata.ts, web-frontend/src/generated/SideConversationStatus.ts, web-frontend/src/generated/TurnVisibility.ts]
behavior_refs: [behavior.side-conversation-lifecycle]
rule_refs: [rule.conversation-collaboration-authorities]
evidence_refs: [evidence.side-conversation-preflight]
finding_refs: []
candidate_refs: []
---

# EKO Conversation collaboration authorities

## 资产身份

该资产集合表示 EKO 已有 Conversation transcript、AgentRouter group/inbox、durable input、foreground turn、event、deletion 及 conversation archive 可见性权威，不创建新的统一 store。

## 来源与消费者

app-core 拥有服务和文件边界；Tauri 与 React 只消费 typed service 与事件投影。TUI、CLI/JSONL 和 channel 不消费 Side Conversation service。

## 生命周期

Conversation create/restore、input persist/drain/settle、turn cancel、internal delivery、archive/delete 与 workspace shutdown 各由现有 owner 结算。

## 候选关系

GUI branch 和 TUI `/fork` 是不同用户意图的既有普通分叉能力，不发布 Side Conversation relation，也不需要收敛到 Side Conversation service。

## 未知与限制

Side Conversation member metadata、模型覆盖、级联关系和 GUI 投影已有本地门禁与独立复审证据；真实 Tauri 窗口验收和远端主分支交付仍需单独确认。
