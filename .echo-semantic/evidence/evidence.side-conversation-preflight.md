---
schema_version: 1
id: evidence.side-conversation-preflight
kind: evidence
observed_at: source:87791a9dc0b2bb4484fff726fea064bd943e080f4f0e97b74d9d5821442c1e9a
source_refs:
  - docs/supreme/specs/side-conversation/design.md
  - docs/supreme/specs/side-conversation/plans/plan_02_gui-side-conversation.md
  - echo-agent-app-core/src/lib.rs
  - echo-agent-app-core/src/side_conversation.rs
  - echo-agent-app-core/src/agent_control.rs
  - echo-agent-app-core/src/agent_router/router.rs
  - echo-agent-app-core/src/conversation_input.rs
  - echo-agent-app-core/src/foreground_turn.rs
  - echo-agent-app-core/src/conversation_deletion.rs
  - echo-agent-app-core/src/workspace/runtime.rs
  - echo-agent-app-core/src/chat_driver.rs
  - echo-agent-app-core/src/state/tests.rs
  - echo-agent-app-core/src/state/config.rs
  - src/tauri/commands/conversations.rs
  - src/tauri/commands/chat.rs
  - src/tauri/commands/agent_router.rs
  - web-frontend/src/stores/conversationStore.ts
  - web-frontend/src/components/chat/ChatPanel.tsx
  - web-frontend/src/components/chat/MessageBubble.completed.test.tsx
  - web-frontend/src/components/layout/LeftSidebar.tsx
  - web-frontend/src/api/endpoints.sideConversation.test.ts
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

来源覆盖 app-core Conversation/Agent collaboration、Tauri chat/conversation commands、GUI 左侧会话树/对话面，以及用户确认并校验的 GUI-only Design 与 schema v4 Plan。

## 已知缺口

app-core 13 项 Side Conversation、Tauri conversation 10 项、foreground turn 6 项和前端相关 23 项定向测试通过。最终 `cargo fmt`、两组 clippy、workspace all-features（app-core 1558、CLI 280、main 11、JSONL subprocess 6 及 doctests）、app-core no-default、GUI check/test（CLI 211、GUI main 1、JSONL subprocess 6）全部通过；前端 Prettier、ESLint、55 个文件 283 项测试、TypeScript/Vite build 通过。Playwright 在 1280×800 与 390×844 无横向溢出，双语文档 parity、非 GUI 源码零差异、严格语义校验与独立 Review 通过。门禁同时修复并验证了 workspace shutdown debt seal 和一个仅测试路径的 5 秒负载敏感 deadline。
