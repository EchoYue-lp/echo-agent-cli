---
schema_version: 1
id: asset.eko-project-workspaces
kind: asset
title: Existing Workspace location, project I/O and model context consumers
asset_type: state_authority
status: needs_review
risk: high
observed_at: source:f49ec6694fa4b4cf0f175f3a746b2a82a610bfd044d86e11e72f88763aa54c7c
boundary_refs:
- boundary.eko-framework-consumer-settlement
behavior_refs: []
rule_refs:
- rule.eko-project-location-consistency
- rule.eko-framework-consumer-authorities
evidence_refs:
- evidence.eko-project-workspaces-source
finding_refs: []
candidate_refs: []
code_refs:
- echo-agent-app-core/src/turn_context.rs
- echo-agent-app-core/src/chat_driver.rs
- echo-agent-app-core/Cargo.toml
- echo-agent-app-core/src/agent_pool/admission.rs
- echo-agent-app-core/src/agent_pool/generation.rs
- echo-agent-app-core/src/agent_pool/pool.rs
- echo-agent-app-core/src/analysis_runtime.rs
- echo-agent-app-core/src/channel_runtime.rs
- echo-agent-app-core/src/config_watcher.rs
- echo-agent-app-core/src/diff.rs
- echo-agent-app-core/src/evolution/review_integration.rs
- echo-agent-app-core/src/extension_control/service.rs
- echo-agent-app-core/src/hook_config_loader.rs
- echo-agent-app-core/src/infra/diagnostics.rs
- echo-agent-app-core/src/infra/factory.rs
- echo-agent-app-core/src/instruction_provider.rs
- echo-agent-app-core/src/plugin_components.rs
- echo-agent-app-core/src/plugin_runtime/runtime.rs
- echo-agent-app-core/src/product_data_io.rs
- echo-agent-app-core/src/project/agent_adapter.rs
- echo-agent-app-core/src/project/context.rs
- echo-agent-app-core/src/project/gitignore.rs
- echo-agent-app-core/src/project/mod.rs
- echo-agent-app-core/src/project/prompt.rs
- echo-agent-app-core/src/runtime.rs
- echo-agent-app-core/src/skills_hub/project.rs
- echo-agent-app-core/src/state/app_state.rs
- echo-agent-app-core/src/state/workspace.rs
- echo-agent-app-core/src/subagent_loader.rs
- echo-agent-app-core/src/tasks/task_runtime/command_cells.rs
- echo-agent-app-core/src/tasks/task_runtime/completion_gate.rs
- echo-agent-app-core/src/tasks/task_runtime/event_rebuild.rs
- echo-agent-app-core/src/tasks/task_runtime/execution_target.rs
- echo-agent-app-core/src/tasks/task_runtime/executor/dispatch.rs
- echo-agent-app-core/src/tasks/task_runtime/executor/events.rs
- echo-agent-app-core/src/tasks/task_runtime/executor/run.rs
- echo-agent-app-core/src/tasks/task_runtime/executor/unattended.rs
- echo-agent-app-core/src/tasks/task_runtime/mod.rs
- echo-agent-app-core/src/tasks/task_runtime/revisioned_runtime.rs
- echo-agent-app-core/src/tasks/task_runtime/store/runtime.rs
- echo-agent-app-core/src/tasks/task_runtime/store/workspace.rs
- echo-agent-app-core/src/tasks/task_runtime/task_execute_tool.rs
- echo-agent-app-core/src/tasks/task_runtime/task_tools.rs
- echo-agent-app-core/src/tasks/task_runtime/types.rs
- echo-agent-app-core/src/tasks/task_runtime/worktree.rs
- echo-agent-app-core/src/terminal.rs
- echo-agent-app-core/src/terminal/project_ssh.rs
- echo-agent-app-core/src/terminal/project_ssh/effective.rs
- echo-agent-app-core/src/terminal/project_ssh/sandbox.rs
- echo-agent-app-core/src/terminal/project_ssh/sftp.rs
- echo-agent-app-core/src/unified_memory.rs
- echo-agent-app-core/src/utils.rs
- echo-agent-app-core/src/workspace/mod.rs
- echo-agent-app-core/src/workspace/registry.rs
- echo-agent-app-core/src/workspace/runtime.rs
- src/main.rs
- src/tauri/commands/files.rs
- src/tauri/commands/panels.rs
- src/tauri/commands/workspace.rs
- src/tauri/desktop.rs
- src/tauri/mod.rs
- src/tauri/terminal.rs
- web-frontend/src/api/endpoints.ts
- web-frontend/src/components/coding/WorktreePanel.tsx
- web-frontend/src/components/terminal/Terminal.tsx
- web-frontend/src/components/terminal/TerminalDrawer.tsx
- web-frontend/src/components/workspace/NewTaskDialog.tsx
- web-frontend/src/components/workspace/ProjectSourceFields.tsx
- web-frontend/src/stores/workspaceStore.ts
consumer_refs:
- echo-agent-app-core/tests/runtime_state_e2e.rs
- web-frontend/src/generated/ProjectExecutionBinding.ts
- web-frontend/src/generated/TaskExecutionTarget.ts
- docs/en/adr/0051-ssh-terminal-sessions.md
- docs/en/adr/0055-project-workspaces.md
- docs/en/architecture/runtime.md
- docs/zh/adr/0051-ssh-terminal-sessions.md
- docs/zh/adr/0055-project-workspaces.md
- docs/zh/architecture/runtime.md
- echo-agent-app-core/src/infra/tests.rs
- echo-agent-app-core/src/state/tests.rs
- echo-agent-app-core/src/tasks/task_runtime/executor/tests.rs
- echo-agent-app-core/src/tasks/task_runtime/project_environment_tests.rs
- echo-agent-app-core/src/tasks/task_runtime/store/tests.rs
- web-frontend/src/api/endpoints.memory.test.ts
- web-frontend/src/components/coding/WorktreePanel.test.tsx
- web-frontend/src/components/terminal/TerminalDrawer.test.tsx
- web-frontend/src/components/workspace/ProjectSourceFields.test.tsx
- web-frontend/src/components/workspace/WorkspaceDialogs.a11y.test.tsx
---

# Existing Workspace project consumers

## 资产身份

WorkspaceRegistry manifest/index and the original owned Workspace transition remain the only location/version authority. ProjectLocation and creation intents are values in that authority; ScopedProjectResources borrows the original host control/I/O receipt. PreparedProjectContext owns one read result, not a store or scheduler. Original TaskRuntime, TerminalService and ApplicationLifecycleOwner retain their claims, receipts, physical settlement and recovery obligations. Framework FileSystem/SandboxExecutor supply generic I/O; EKO owns location policy and presentation.

## 来源与消费者

The mounted Workspace creation form (historical file name NewTaskDialog) sends an explicit source through workspaceStore and the existing create/switch IPC. Sidebar New task starts the original Conversation draft and creates no directory. Existing/new local directories remain distinct from the local product-data root. Configured SSH location and its verified execution snapshot share the manifest/revision. Host and pool resources supply the same target to original tools, required pre-model loaders, GUI file/diff/Git/worktree calls and project terminals. GUI requests retain explicit Workspace ID and generation; neither callbacks nor providers reread current sidebar focus to choose a target.

Required context preparation is fallible before delegation, with the actual isolated cwd. Fresh default and Plugin Subagent factories use the original invocation API; immutable prepared values publish through the existing pre-model projector after reset/restore, replacing older checkpoint envelopes by their original markers. The role root stays independent and no fallible I/O is deferred to a warning-only projector. The primary's existing EkoContextProjector is not replaced. User/global configuration and product data retain local authorities. Plain SSH is not an OS-sandbox receipt. Uncertain writes, cancellation observers and physical cleanup do not manufacture a completed result.

The same original turn registration carries each primary invocation's prepared project values. Its EKO projector preserves Domain/TaskRuntime, hot memory, turn policy and command watches after restore. Ordinary run-less turns are covered without deriving a TaskRun. Independent primary verifications use existing execution IDs while retaining their root-message trigger. Fresh Subagents compose only their own actual checkout values with the original Domain-only renderer. Project and Domain identities remain separate fields in the same RunCreated fold and revisioned task extension.

## 生命周期

Original Workspace creation publishes its manifest/index once; original owned switching and idle host retirement govern changes. Borrowed project I/O receipts retain the captured generation through physical completion. Auth drafts either transfer to the published Workspace or close through TerminalService. Human terminal shutdown precedes host I/O joins; shared masters close after their original consumers settle.

## 候选关系

This candidate extends the existing Workspace and original Framework provider boundary. It replaces local-only consumption at the listed entry points rather than introducing a second project registry, loader, task graph or tool set.

## 未知与限制

The current source is an implementation candidate. Source references include the actual files/diff consumers that previously lacked Project-specific mapping. The related Rule is needs_review. Focused frontend epoch2 ran 28 tests across five files, whole-app noEmit TS, scoped ESLint and format successfully. No Rust/native/real-remote qualification is claimed here; excluded retirement-tool repair, old Findings and accepted dependency risks are unchanged. ADR0055 records the confirmed direction, not a released main commit.
