---
schema_version: 1
id: map.eko-conversation-collaboration
kind: capability_map
title: EKO Conversation collaboration
risk: high
observed_at: 40fd80447cdf3d7627399af0b2069a8b4da5327a
boundary_refs: [boundary.eko-conversation-collaboration]
behavior_refs: [behavior.side-conversation-lifecycle, behavior.gui-conversation-fork, behavior.automatic-agent-collaboration-ui-boundary]
rule_refs: [rule.conversation-collaboration-authorities]
evidence_refs: [evidence.side-conversation-preflight, evidence.gui-conversation-fork, evidence.automatic-agent-collaboration-ui-retirement]
finding_refs: [finding.manual-agent-collaboration-ui-retirement]
audit_refs: [audit.automatic-agent-collaboration-ui-retirement]
related_map_refs: []
scenarios:
  ordinary-conversation-gui-fork:
    status: mapped
    source_refs: [src/tauri/commands/conversations.rs, web-frontend/src/api/endpoints.ts, web-frontend/src/stores/conversationStore.ts, web-frontend/src/components/chat/ChatPanel.tsx, web-frontend/src/components/chat/MessageBubble.tsx]
    rule_refs: [rule.conversation-collaboration-authorities]
    evidence_refs: [evidence.gui-conversation-fork]
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
    source_refs: [echo-agent-app-core/src/agent_control.rs, echo-agent-app-core/src/agent_router/router.rs, echo-agent-app-core/src/state/app_state.rs, web-frontend/src/components/chat/ChatPanel.tsx, src/tauri/mod.rs]
    rule_refs: [rule.conversation-collaboration-authorities]
    evidence_refs: [evidence.side-conversation-preflight, evidence.automatic-agent-collaboration-ui-retirement]
  side-conversation-create-and-recover:
    status: mapped
    source_refs: [docs/supreme/specs/side-conversation/design.md, echo-agent-app-core/src/side_conversation.rs, echo-agent-app-core/src/conversation_archive.rs, echo-agent-app-core/src/managed_conversation.rs, echo-agent-app-core/src/state/app_state.rs, echo-agent-app-core/src/state/workspace.rs, echo-agent-app-core/src/state/tests.rs, src/tauri/commands/conversations.rs, web-frontend/src/stores/conversationStore.ts, web-frontend/src/generated/TurnVisibility.ts]
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

GUI 通过 Tauri conversation commands 创建和管理 Side Conversation，并通过同一 branch command 将完成回复 Fork 为列表中的独立普通会话。模型侧 Agent control 继续负责内部消息；TUI、CLI/JSONL 和 channel 不增加 Side Conversation 专用入口。输出为 conversation transcript、typed receipt、事件投影和 GUI 左侧会话树。

## 行为关系

ConversationStore 保存 transcript，AgentRouter 保存内部 inbox 与 group，ForegroundTurnControl 保存活跃 turn；EKO 既有 conversation archive 文件保存界面可见性和 handoff 意图。Side Conversation 只能组合这些权威。

## 状态与数据流

稳定地址是 workspace 与 conversation，活跃 turn 另带 root/active turn identity。Side Conversation 父子关系不得成为 TaskRun 或 PlanTask 状态。

## 策略来源与优先级

仓库 AGENTS、Side Conversation Design、现有 Conversation/Agent collaboration ADR 与 typed runtime contract 决定行为；用户确认的一级拓扑和无自动回写优先。

## 生命周期与失败路径

覆盖 create、revision/visibility 一致性 snapshot、first dispatch、continue、cancel、rename、delete、restart recovery 和 relation cleanup；内部消息的可见性在 Router terminal 前结算，部分写入必须有明确回执或补偿。

## 权限与敏感信息

Side Conversation 不增加本地权限门控。模型选择和内部消息不得把密钥写入日志，删除必须避免无意数据丢失。

## 用户侧投影

GUI 左侧树和完整对话表面呈现 Side Conversation。依据用户提供的 Codex 参考，普通对话时间线只读呈现 typed Agent 来信与原工具发送事实：用户输入和 Agent 来信均右对齐，来信保留来源且可展开全文，发送行点击只导航到原目标会话。来源未知时明确显示未提供，不能猜成人类；canonical 接收时间与来源创建时间分别保留。GUI 不新增人工消息编排或投递收据管理台，Agent 来信不能批准 HITL、更新用户 Goal 或取得本人编辑权限。

### 开发测试中的原生时间线接缝

`web-frontend/src/testing/nativeChatTimeline.tsx` 只供性能测试入口导入，复用原事件 handler、ChatStore 和 MessageBubble／原生消息／工具组件，不进入 shipping index，也不拥有投递、持久化或完成权威。`web-frontend/src/testing/nativeChatTimeline.test.tsx` 在现有 Vitest 链验证该适配边界；其导出是开发测试 ABI，不是新增产品 API。输入、提交与见证的精确边界见 `docs/zh/architecture/performance-baseline.md`：JS 回归、DOM-ready、React layout-effect 和 WebKit transaction 都不能授予实际屏幕 paint 或完整原生性能资格；真实 capture、工具轨迹、冻结二进制及独占测量仍须分别取证。

后端离线捕获仍由 `src/tauri/commands/native_timeline_actual_backend_capture.rs` 的原显式测试入口驱动，复用 `src/tauri/commands/conversations_native_history_tests.rs` 的资源链 producer；`src/tauri/commands/native_timeline_tool_backend_capture.rs` 只观察原 `bind_surface_chat_sink(Gui)` 之后的 journal／工具摘要回调，保其交错顺序、身份和真实 publication。新增模块不创建产品协议、renderer owner 或测量 runner。当前采用仅有源码审查和静态检查；准确最终 Framework／GUI 制品上的编译、五组真实原件、private reader、App／capability／quiet／paint 资格分别取得，不能沿用旧 unknown-only capture 或其它来源的绿色结果。

## 场景处置清单

现有 transcript、input/cancel、AgentRouter 消息、GUI Side Conversation 创建/恢复和 GUI-only surface 边界均已映射并通过适用工程门禁与独立 Review。

## 未展开项

TaskRun formal Subagent、跨 Workspace handoff、默认 worktree 隔离和自动代码合并不属于本边界。
