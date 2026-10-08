---
schema_version: 1
id: finding.manual-agent-collaboration-ui-retirement
kind: finding
type: consolidation_candidate
status: resolved
severity: high
primary_focus: trigger_input
focus: [state_authority, contract_evidence]
boundary_ref: boundary.eko-conversation-collaboration
behavior_refs: [behavior.automatic-agent-collaboration-ui-boundary]
rule_refs: [rule.conversation-collaboration-authorities]
evidence_refs: [evidence.automatic-agent-collaboration-ui-retirement]
audit_refs: [audit.automatic-agent-collaboration-ui-retirement]
decision_refs: [docs/en/adr/0047-automatic-agent-collaboration.md]
repair_evidence_refs: [evidence.automatic-agent-collaboration-ui-retirement]
verification_evidence_refs: [evidence.automatic-agent-collaboration-ui-retirement]
rereview_audit_refs: [audit.automatic-agent-collaboration-ui-retirement]
discovered_at: 8bd2a6f08117cde61037b583000c71ab820e73ce
decision: retire
candidate_asset_refs: [asset.manual-gui-agent-controls, asset.eko-conversation-authorities]
canonical_asset_ref: asset.eko-conversation-authorities
replacement_refs: [asset.eko-conversation-authorities]
delete_paths: [web-frontend/src/components/chat/AgentMessageDialog.tsx, web-frontend/src/components/chat/AgentMessageDialog.test.tsx, web-frontend/src/components/chat/AgentGroupPanel.tsx, src/tauri/commands/agent_router.rs]
rollback_ref: 8bd2a6f08117cde61037b583000c71ab820e73ce
---

# Retire manual Agent collaboration GUI

## 问题

用户被要求人工组织本应由模型工具完成的协作。将其改成动态管理台仍不符合用户确认的产品边界。

## 触发条件与影响

人工选地址、发消息和组 CRUD 形成另一条产品操作流程，但不是核心 Agent 能力的权威。

## 证据

删除前历史 asset 绑定 8bd2a6f；现有 AgentControlService、AppState 与 TaskRuntime 是保留的 canonical 能力。

## 处理记录

用户明确授权退役此 GUI 管理面，ADR 0047 记录决定。删除专用 UI/adapter，撤回未提交动态查询；不删除核心工具或数据。等价验证只覆盖保留的自动协作，不包括有意退役 UI。适用门禁全部通过，主代理按冻结源码与实际验证复核后结算本 Finding；不声称独立复审。
