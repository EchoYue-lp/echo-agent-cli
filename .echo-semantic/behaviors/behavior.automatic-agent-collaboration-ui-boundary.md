---
schema_version: 1
id: behavior.automatic-agent-collaboration-ui-boundary
kind: behavior
status: verified
expectation: human_confirmed
risk: high
primary_focus: trigger_input
focus: [state_authority, contract_evidence]
boundary: boundary.eko-conversation-collaboration
observed_at: 40fd80447cdf3d7627399af0b2069a8b4da5327a
code_refs: [AGENTS.md, README.md, docs/en/features.md, docs/zh/features.md, docs/en/adr/0047-automatic-agent-collaboration.md, docs/zh/adr/0047-automatic-agent-collaboration.md, web-frontend/src/components/chat/ChatPanel.tsx, web-frontend/src/api/endpoints.ts, src/tauri/commands/mod.rs, src/tauri/mod.rs, echo-agent-app-core/src/agent_control.rs, echo-agent-app-core/src/state/app_state.rs, echo-agent-app-core/src/tasks/task_runtime/subagent_control.rs]
rule_refs: [rule.conversation-collaboration-authorities]
evidence_refs: [evidence.automatic-agent-collaboration-ui-retirement]
finding_refs: [finding.manual-agent-collaboration-ui-retirement]
---

# Automatic Agent collaboration GUI boundary

## 重要承诺

Agent 自主使用工具和运行时组织通信与组编排，GUI 不要求用户手动管理协作；用户通过 Sidechat/Forkchat 参与探索。

## 当前行为

人工协作弹窗、组编辑器、专用前端 API 与 Tauri commands 已删除。原模型工具、AgentRouter/AppState、TaskRuntime 路径保留。

## 期望行为

不新增协作动态查询或替代管理页面。Sidechat 创建/导航、Fork 完整回合复制和既有任务/HITL 呈现保持原语义。

## 触发、结果与副作用

模型使用 agent_* 与 task_* 工具实施协作；GUI 的 Side/Fork 仍是用户显式会话操作。本次删除不清空用户数据。

## 失败、重试与恢复

投递、取消、结果结算、重试与恢复继续归既有 owner；没有新 GUI 观察器或状态权威。

## 证据

本次保留核心协作的验证与有意退役人工 UI 分开记录；适用 Rust/GUI/frontend 门禁已全部通过。

## 裁决记录

用户明确纠正：Agent 协作由 AI 自动进行，用户不需要管理；Sidechat 和 Forkchat 才用于用户参与。ADR 0047 为此退役的产品权威。
