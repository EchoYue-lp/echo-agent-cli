---
schema_version: 1
id: audit.automatic-agent-collaboration-ui-retirement
kind: audit
boundary_ref: boundary.eko-conversation-collaboration
lens: trigger_input
freshness: examined
revision: source:f96d8ee7fbc893913ba410ee992bdeb7e906fefbab84df09ef7719bf62dc79b3
finding_refs: [finding.manual-agent-collaboration-ui-retirement]
challenges:
  runtime-tools-removed-with-ui:
    revision: source:f96d8ee7fbc893913ba410ee992bdeb7e906fefbab84df09ef7719bf62dc79b3
    source_refs: [echo-agent-app-core/src/agent_control.rs, echo-agent-app-core/src/state/app_state.rs, echo-agent-app-core/src/tasks/task_runtime/subagent_control.rs]
    evidence_refs: [evidence.automatic-agent-collaboration-ui-retirement]
  side-fork-entry-lost:
    revision: source:f96d8ee7fbc893913ba410ee992bdeb7e906fefbab84df09ef7719bf62dc79b3
    source_refs: [web-frontend/src/components/chat/ChatPanel.tsx, web-frontend/src/components/chat/MessageBubble.tsx]
    evidence_refs: [evidence.automatic-agent-collaboration-ui-retirement]
---

# GUI retirement candidate review

## 审查范围

主代理源码与候选验证复核，不声称独立 Review。

## 已检查故障假设

检查删除 GUI adapter 是否误删模型工具/运行时，以及 Side/Fork 是否仍可交互。

## 实际实现路径与证据

Core 工具路径源码与删除前一致，原始 8bd2a6f 的 AgentControl 31 项通过；当前前端 287 项及当前 dependency graph 的 workspace all-feature 1594 app-core/280 CLI 均通过。被删除 API 的唯一前端消费者属于同一退役 GUI；Core/generated 类型、TUI/CLI/channel 控制及 Side/Fork 保留。

## 问题记录

原动态管理台方向已按用户纠正撤回。

## 残余风险

保留的旧 journal 数据与当前格式不匹配不是本次 UI 退役修复范围。

## 未检查项

原生新构建窗口、最终 Rust 门禁与远端候选交付尚待结算。
