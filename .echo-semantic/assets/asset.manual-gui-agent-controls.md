---
schema_version: 1
id: asset.manual-gui-agent-controls
kind: asset
title: Retired manual GUI collaboration controls
asset_type: entrypoint
status: deprecated
risk: high
observed_at: 8bd2a6f08117cde61037b583000c71ab820e73ce
boundary_refs: [boundary.eko-conversation-collaboration]
code_refs: [web-frontend/src/components/chat/AgentMessageDialog.tsx, web-frontend/src/components/chat/AgentMessageDialog.test.tsx, web-frontend/src/components/chat/AgentGroupPanel.tsx, src/tauri/commands/agent_router.rs]
consumer_refs: [web-frontend/src/components/chat/ChatPanel.tsx, web-frontend/src/api/endpoints.ts, src/tauri/mod.rs]
behavior_refs: [behavior.automatic-agent-collaboration-ui-boundary]
rule_refs: [rule.conversation-collaboration-authorities]
evidence_refs: [evidence.automatic-agent-collaboration-ui-retirement]
finding_refs: [finding.manual-agent-collaboration-ui-retirement]
candidate_refs: [asset.eko-conversation-authorities]
---

# Retired manual collaboration surface

## 资产身份

仅表示删除前的人工 GUI 消息/组管理与其专用 adapter，绑定可恢复的历史提交，不是当前入口。

## 来源与消费者

历史 ChatPanel、前端 agentApi 和 Tauri registry 曾消费该资产。

## 生命周期

按用户产品决定整体退役；其 model/runtime 能力由原 canonical authority 保留。

## 候选关系

canonical asset 为既有 Conversation/Agent authorities；不把另一个人工 GUI 作为替代。

## 未知与限制

不声称人工 UI 行为等价保留。保留性验证仅覆盖自动协作核心及 Side/Fork。
