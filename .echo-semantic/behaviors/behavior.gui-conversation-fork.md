---
schema_version: 1
id: behavior.gui-conversation-fork
kind: behavior
status: verified
expectation: human_confirmed
risk: high
primary_focus: time_lifecycle
focus: [state_authority, data_durability, failure_concurrency, contract_evidence]
boundary: boundary.eko-conversation-collaboration
observed_at: source:e310dee0cb79fe7ee6d4072248885201202320e426d217bcf97b3e5e600558a0
code_refs: [README.md, docs/en/features.md, docs/zh/features.md, docs/en/adr/0045-managed-conversation-lifecycle.md, docs/zh/adr/0045-managed-conversation-lifecycle.md, src/tauri/commands/conversations.rs, web-frontend/src/api/endpoints.ts, web-frontend/src/api/endpoints.sideConversation.test.ts, web-frontend/src/stores/conversationStore.ts, web-frontend/src/stores/conversationStore.test.ts, web-frontend/src/components/chat/ChatPanel.tsx, web-frontend/src/components/chat/MessageBubble.tsx, web-frontend/src/components/chat/MessageBubble.fork.test.tsx]
rule_refs: [rule.conversation-collaboration-authorities]
evidence_refs: [evidence.gui-conversation-fork]
finding_refs: []
---

# GUI conversation Fork

## 重要承诺

GUI 完成回复的操作区有独立“分叉会话”按钮。它包含该回复所在完整用户回合的 committed history，打开一个列在当前工作区里的普通会话，并保留原会话。

## 当前行为

既有 branch_conversation 用 include_turn 区分完整回合 Fork 和编辑/重新生成的回合前快照。新的会话仍由 ConversationStore、AgentPool 与 managed import 协调器持有；GUI 通过既有 conversationStore 刷新列表并加载新会话。

## 期望行为

Fork 不自动重发问题或调用模型，不建立 Side 关系。完整 canonical 工具历史保留，隐藏内部消息和后续用户回合不进入快照。标题加 `(branch)`，主/分叉会话是列表中的独立条目。

## 触发、结果与副作用

完成回复下方提供复制、重新生成、分叉会话三个按钮。用户点击 Fork 后创建新会话、导入快照、恢复 Agent，随后切换历史视图；原会话内容不变。

## 失败、重试与恢复

源快照读取全程持有 identity 与空闲准入；忙回合与未完成最终回复拒绝分叉。前端关闭重复点击，后端导入失败沿既有聚合删除回滚目标。工作区/会话选择改变时，迟到响应不能覆盖新选择。

## 证据

按钮事件、列表刷新/加载、迟到响应、错误保留源会话、完整工具回合与 internal/future 行排除由本次 Evidence 中的测试证明。

## 裁决记录

用户要求借鉴提供的 Codex 回复操作区，新增 Fork 按钮和普通会话列表可见性；Side Conversation 继续是另一种 GUI 父子关系。
