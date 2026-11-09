---
schema_version: 1
id: evidence.eko-current-input-tools-source
kind: evidence
observed_at: source:4060067563e962ddfca747ac2cdc8001657d5dc72e074638e0c4f9c4a038dd47
source_refs:
- echo-agent-app-core/src/tool_control.rs
- echo-agent-app-core/src/conversation_projection.rs
- echo-agent-app-core/src/prepared_turn.rs
- echo-agent-app-core/src/prepared_turn/provenance.rs
- echo-agent-app-core/src/agent_control.rs
- echo-agent-app-core/src/state/app_state.rs
- web-frontend/src/components/chat/AgentInputMessage.tsx
- web-frontend/src/components/chat/AgentSentMessage.tsx
- web-frontend/src/components/chat/ToolExecutionGroup.tsx
- web-frontend/src/components/tools/ToolsPanel.tsx
- web-frontend/src/generated/ConversationInputSource.ts
- web-frontend/src/generated/ToolCategory.ts
- web-frontend/src/generated/ToolSource.ts
- web-frontend/src/hooks/useAgentCommunicationInputs.ts
- web-frontend/src/lib/agentCommunication.ts
- web-frontend/src/lib/openAgentConversation.ts
- docs/zh/adr/0047-automatic-agent-collaboration.md
- docs/zh/adr/0057-native-messages-and-progress-policy.md
- docs/zh/adr/0061-tool-categories-and-required-tools.md
supports:
- rule.eko-framework-consumer-authorities
- rule.conversation-collaboration-authorities
limitations:
- 仅本轮当前源码与前端消费者依据，不是Rust或真实后端发布/原生UI通过
- 普通Evidence，无evidence_type/scenario_results/after_revision，不把旧版本验收改matched
- 旧历史证据、未决needs_review、原失败和外部source402工具合同保持，不改工具或排除
- NoCompat模块删除及两个无native Fork旁路的当前后端验证/准入仍需后继精确批次
---

# 当前 typed 输入与工具控制源码依据

## 来源与范围

本观察绑定原工具 source_digest 对当前 CLI 工作树的实际只读结果。跨会话事实由原
AgentRouter 消息、投递记录及当前 Message 中的原 ResourceLink 提供，工具变化由原
ToolControl snapshot／revision 持有；本观察不新增执行或消息权威。

## 支持的结论

- `echo-agent-app-core/src/tool_control.rs` 只持 EKO 必需工具策略、六组显示分类与来源适配；
  现 `ToolControlService` 仍拥有 revisioned 选择。内置可选工具不因类别变必需；来源与分类正交。
- `web-frontend/src/components/tools/ToolsPanel.tsx` 只在原 scope／generation／view epoch 内批量
  操作可选项，错误后重新读取原权威目录，保持实际错误，不重试 mutation 或回滚已发生选择。
- `web-frontend/src/generated/ToolCategory.ts` 与 `web-frontend/src/generated/ToolSource.ts` 是原
  ToolInfo 的分类／来源 DTO，不能扩大执行许可、profile 或只读正上界。
- `web-frontend/src/generated/ConversationInputSource.ts` 只保留当前 GUI／channel 输入来源；
  TUI／CLI 旧产品入口枚举随具名退役退出。字段移除不改变原 input_id/CAS/Goal 权威。
- `echo-agent-app-core/src/prepared_turn.rs`、`echo-agent-app-core/src/prepared_turn/provenance.rs`
  和 `echo-agent-app-core/src/conversation_projection.rs` 保留当前 typed AgentMessage 的原始
  message_id、来源、目标和正文；不解析旧 wrapper 或从普通文本猜来源。
- `web-frontend/src/lib/agentCommunication.ts` 与 `web-frontend/src/hooks/useAgentCommunicationInputs.ts`
  只合并同一消息原身份的只读视图；Accepted、drained、结果分别呈现，不改 canonical Goal。
- `web-frontend/src/components/chat/AgentInputMessage.tsx` 与原 MessageBubble 把用户输入和
  Agent 输入右对齐，但 Agent 输入保持来源标记，没有用户编辑／重新生成／Fork 权限。
- `web-frontend/src/components/chat/AgentSentMessage.tsx` 与
  `web-frontend/src/components/chat/ToolExecutionGroup.tsx` 沿原 transcriptToolExecution 核对
  owner/call 与详情 ID。目标只来自 typed result.data；失败保原因与原详情，不能解析 output
  字符串造目标。`web-frontend/src/lib/openAgentConversation.ts` 复用原导航准入、registry、
  generation 与视图 fence；点击不发送、不创建或执行目标会话。

当前组合的前端整套、类型、格式与生产构建另有真实命令原件；这些只证明相应前端范围，
不续期旧 Rust／native／依赖风险结果。当前 ToolInfo 后端、原 producer cold/Fork 与实际 DTO
仍必须在 Root 后继发布窗编译和执行，不按测试定义或静态审查冒通过。

## 已知缺口

Framework／CLI 当前完整矩阵、真实原生 App、SSH、微信手机收件、main CI 和发布均未由
本证据完成。两个 Fork 旧放行旁路仍先保留到非流式正控实跑后同批严格删除；legacy getter
删除不把取消的旧兼容要求算通过。已有语义材料中的旧源码引用或 repairRefs 缺口保原失败，
不以当前普通 Evidence 掩盖。
