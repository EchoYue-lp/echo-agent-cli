---
schema_version: 1
id: map.eko-conversation-collaboration
kind: capability_map
title: EKO Conversation collaboration
risk: high
observed_at: source:87791a9dc0b2bb4484fff726fea064bd943e080f4f0e97b74d9d5821442c1e9a
boundary_refs: [boundary.eko-conversation-collaboration]
behavior_refs: [behavior.side-conversation-lifecycle]
rule_refs: [rule.conversation-collaboration-authorities]
evidence_refs: [evidence.side-conversation-preflight]
finding_refs: []
audit_refs: []
related_map_refs: []
scenarios:
  conversation-transcript-persistence:
    status: mapped
    source_refs: [src/tauri/commands/conversations.rs, echo-agent-app-core/src/state/config.rs]
    rule_refs: [rule.conversation-collaboration-authorities]
    evidence_refs: [evidence.side-conversation-preflight]
  conversation-input-and-cancel:
    status: mapped
    source_refs: [src/tauri/commands/chat.rs, echo-agent-app-core/src/conversation_input.rs, echo-agent-app-core/src/foreground_turn.rs]
    rule_refs: [rule.conversation-collaboration-authorities]
    evidence_refs: [evidence.side-conversation-preflight]
  agent-router-internal-messaging:
    status: mapped
    source_refs: [echo-agent-app-core/src/agent_control.rs, echo-agent-app-core/src/agent_router/router.rs, src/tauri/commands/agent_router.rs]
    rule_refs: [rule.conversation-collaboration-authorities]
    evidence_refs: [evidence.side-conversation-preflight]
  side-conversation-create-and-recover:
    status: mapped
    source_refs: [docs/supreme/specs/side-conversation/design.md, echo-agent-app-core/src/side_conversation.rs, echo-agent-app-core/src/state/app_state.rs, echo-agent-app-core/src/state/tests.rs, src/tauri/commands/conversations.rs, web-frontend/src/stores/conversationStore.ts]
    rule_refs: [rule.conversation-collaboration-authorities]
    evidence_refs: [evidence.side-conversation-preflight]
  side-conversation-gui-boundary:
    status: mapped
    source_refs: [AGENTS.md, README.md, docs/supreme/specs/side-conversation/design.md, src/tauri/commands/conversations.rs, web-frontend/src/api/endpoints.sideConversation.test.ts, web-frontend/src/components/layout/LeftSidebar.tsx, web-frontend/src/components/chat/ChatPanel.tsx, web-frontend/src/components/chat/MessageBubble.tsx, src/tui/events.rs, src/cli/jsonl.rs, src/cli/channels.rs]
    rule_refs: [rule.conversation-collaboration-authorities]
    evidence_refs: [evidence.side-conversation-preflight]
---

# EKO Conversation collaboration

## 能力范围

覆盖 EKO 会话持久化、conversation-scoped Agent 执行、输入与取消、Conversation Agent 内部消息，以及 GUI 使用的一级 Side Conversation 产品关系。

## 入口与输出

GUI 通过 Tauri conversation commands 创建和管理 Side Conversation，模型侧 Agent control 继续负责内部消息；TUI、CLI/JSONL 和 channel 不增加 Side Conversation 专用入口。输出为 conversation transcript、typed receipt、事件投影和 GUI 左侧会话树。

## 行为关系

ConversationStore 保存 transcript，AgentRouter 保存内部 inbox 与 group，ForegroundTurnControl 保存活跃 turn；Side Conversation 只能组合这些权威。

## 状态与数据流

稳定地址是 workspace 与 conversation，活跃 turn 另带 root/active turn identity。Side Conversation 父子关系不得成为 TaskRun 或 PlanTask 状态。

## 策略来源与优先级

仓库 AGENTS、Side Conversation Design、现有 Conversation/Agent collaboration ADR 与 typed runtime contract 决定行为；用户确认的一级拓扑和无自动回写优先。

## 生命周期与失败路径

覆盖 create、snapshot、first dispatch、continue、cancel、rename、delete、restart recovery 和 relation cleanup；部分写入必须有明确回执或补偿。

## 权限与敏感信息

Side Conversation 不增加本地权限门控。模型选择和内部消息不得把密钥写入日志，删除必须避免无意数据丢失。

## 用户侧投影

GUI 左侧树和完整对话表面呈现 Side Conversation；TUI、CLI/JSONL 与 channel 保持既有普通 conversation/Subagent 语义。内部消息默认不插入主 GUI transcript。

## 场景处置清单

现有 transcript、input/cancel、AgentRouter 消息、GUI Side Conversation 创建/恢复和 GUI-only surface 边界均已映射并通过适用工程门禁与独立 Review。

## 未展开项

TaskRun formal Subagent、跨 Workspace handoff、默认 worktree 隔离和自动代码合并不属于本边界。
