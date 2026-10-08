---
schema_version: 1
id: evidence.side-conversation-preflight
kind: evidence
observed_at: 6b162b6ab2aa13c8262fd6424ff089a692df70b5
source_refs:
  - docs/supreme/specs/side-conversation/design.md
  - docs/supreme/specs/side-conversation/plans/plan_02_gui-side-conversation.md
  - echo-agent-app-core/src/lib.rs
  - echo-agent-app-core/src/side_conversation.rs
  - echo-agent-app-core/src/conversation_archive.rs
  - echo-agent-app-core/src/managed_conversation.rs
  - echo-agent-app-core/src/agent_control.rs
  - echo-agent-app-core/src/agent_router/router.rs
  - echo-agent-app-core/src/conversation_input.rs
  - echo-agent-app-core/src/foreground_turn.rs
  - echo-agent-app-core/src/conversation_deletion.rs
  - echo-agent-app-core/src/workspace/runtime.rs
  - echo-agent-app-core/src/chat_driver.rs
  - echo-agent-app-core/src/state/tests.rs
  - echo-agent-app-core/src/state/workspace.rs
  - echo-agent-app-core/src/state/config.rs
  - src/tauri/commands/conversations.rs
  - src/tauri/commands/chat.rs
  - src/tauri/commands/agent_router.rs
  - web-frontend/src/stores/conversationStore.ts
  - web-frontend/src/components/chat/ChatPanel.tsx
  - web-frontend/src/components/chat/MessageBubble.completed.test.tsx
  - web-frontend/src/components/layout/LeftSidebar.tsx
  - web-frontend/src/api/endpoints.sideConversation.test.ts
  - web-frontend/src/generated/TurnVisibility.ts
  - web-frontend/src/components/chat/SideConversationDialog.test.tsx
supports: [behavior.side-conversation-lifecycle, rule.conversation-collaboration-authorities]
limitations:
  - GUI 布局已由浏览器等价视口截图验证，真实 Tauri 窗口仍依赖目标环境验收
  - transcript 读取与 viewed marker 推进之间仍有极窄窗口，可能漏一个未读 badge，但消息本身不会丢失
---

# Side Conversation preflight evidence

## 支持的结论

当前仓库已有 file-backed Conversation transcript、conversation-scoped AgentPool、durable input、精确 foreground cancel、ConversationDeletionService、AgentRouter inbox/group、GUI branch 和 TUI fork；候选实现只在应用层组合这些权威，并只由 Tauri/GUI 暴露 Side Conversation。

## 来源与范围

来源覆盖 app-core Conversation/Agent collaboration、Tauri chat/conversation commands、GUI 左侧会话树/对话面、内部消息可见性文件、managed 导入，以及用户确认并校验的 GUI-only Design 与 schema v4 Plan。

## 已知缺口

历史 GUI-only 交付已通过 Playwright 的 1280×800 与 390×844 布局检查。本轮新增 Side snapshot 读窗口 barrier、未结算内部消息拒绝、owner-loss 首行未提交与 Router terminal 前可见性结算测试；最终 workspace all-features app-core 1594/1603、CLI 280/280，GUI 211/211，JSONL subprocess 6/6，无失败。前端 Prettier、ESLint、55 个文件 283 项测试、TypeScript/Vite build 通过。真实 Tauri 窗口和远端 main 快照仍不由本地证据证明。
