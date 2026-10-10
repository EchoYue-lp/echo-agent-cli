---
schema_version: 1
id: evidence.automatic-agent-collaboration-ui-retirement
kind: evidence
observed_at: 40fd80447cdf3d7627399af0b2069a8b4da5327a
source_refs: [AGENTS.md, README.md, docs/en/features.md, docs/zh/features.md, docs/en/adr/0047-automatic-agent-collaboration.md, docs/zh/adr/0047-automatic-agent-collaboration.md, web-frontend/src/components/chat/ChatPanel.tsx, web-frontend/src/api/endpoints.ts, src/tauri/commands/mod.rs, src/tauri/mod.rs, echo-agent-app-core/src/agent_control.rs, echo-agent-app-core/src/state/app_state.rs, echo-agent-app-core/src/agent_router/router.rs, echo-agent-app-core/src/tasks/task_runtime/subagent_control.rs, echo-agent-app-core/src/evolution/review_integration.rs, echo-agent-app-core/src/mcp_config_runtime.rs, echo-agent-app-core/src/plugin_runtime/tests.rs, echo-agent-app-core/src/product_data_io.rs, echo-agent-app-core/src/tasks/task_runtime/command_cells.rs, echo-agent-app-core/src/tasks/task_runtime/store/runtime.rs]
supports: [behavior.automatic-agent-collaboration-ui-boundary, rule.conversation-collaboration-authorities]
limitations: [人工 GUI 有意退役，等价范围不包含其交互, 本地候选不是远端合并证据]
evidence_type: behavior_equivalence
before_revision: 8bd2a6f08117cde61037b583000c71ab820e73ce
after_revision: 40fd80447cdf3d7627399af0b2069a8b4da5327a
deleted_paths: [web-frontend/src/components/chat/AgentMessageDialog.tsx, web-frontend/src/components/chat/AgentMessageDialog.test.tsx, web-frontend/src/components/chat/AgentGroupPanel.tsx, src/tauri/commands/agent_router.rs]
coverage: [model control schema and typed lifecycle, Sidechat and Fork user actions, intentional GUI retirement under ADR 0047]
scenario_results:
  retained-agent-core:
    status: matched
    source_refs: [echo-agent-app-core/src/agent_control.rs, echo-agent-app-core/src/state/app_state.rs]
  retained-side-and-fork:
    status: matched
    source_refs: [web-frontend/src/components/chat/MessageBubble.fork.test.tsx, web-frontend/src/components/chat/SideConversationDialog.test.tsx]
command_results:
  - { command: 'before: echo_agent_app_core-156f169795b210a8 agent_control::tests:: --quiet', exit_code: 0 }
  - { command: 'after: npm test && npm run build', exit_code: 0 }
  - { command: 'after: cargo test --workspace --all-features --locked --quiet (Rust 1.99, final dependency graph)', exit_code: 0 }
---

# Automatic collaboration retained; manual GUI retired

## 支持的结论

原始 8bd2a6f 测试二进制的 AgentControl 31 项通过；当前前端 55 文件 287 项、Prettier、ESLint、TypeScript/Vite 构建通过，包含 Sidechat/Fork 正向交互。Core AgentControl/AppState/Router 源码未因 GUI 删除改变。当前 Rust 1.99 fmt、两档 Clippy、workspace all-features（app-core 1594 passed/9 ignored、CLI 280、main 11、JSONL 6）、app-core no-default、GUI check 与 GUI tests（211、Tauri main 1、JSONL 6）全部通过。双语 parity 58 对/48 ADR。

## 来源与范围

删除的四个已提交路径：web-frontend/src/components/chat/AgentMessageDialog.tsx、web-frontend/src/components/chat/AgentMessageDialog.test.tsx、web-frontend/src/components/chat/AgentGroupPanel.tsx、src/tauri/commands/agent_router.rs。人工 UI 按 ADR 0047 有意退役。撤回的未提交动态查询不是新权威，也不进入交付。

## 已知缺口

等价范围仅为保留的自动协作与 Side/Fork，不将已退役人工 UI 伪装为等价保留。所有适用候选门禁通过，但不宣称远端合并或旧用户 journal 已恢复。初轮消费者检查遇到框架移除宏依赖后的 lock 更新要求，已离线刷新单个依赖边并完整复验。
