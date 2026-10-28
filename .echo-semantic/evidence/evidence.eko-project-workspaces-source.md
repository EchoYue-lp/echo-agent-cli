---
schema_version: 1
id: evidence.eko-project-workspaces-source
kind: evidence
observed_at: source:f49ec6694fa4b4cf0f175f3a746b2a82a610bfd044d86e11e72f88763aa54c7c
source_refs:
- web-frontend/src/generated/DomainRoleBinding.ts
- web-frontend/src/generated/DomainPluginIdentity.ts
- web-frontend/src/generated/DomainBindingRef.ts
- echo-agent-app-core/src/tasks/task_runtime/profiles.rs
- echo-agent-app-core/src/subagent_prompt.rs
- echo-agent-app-core/src/domain_plugin.rs
- docs/zh/adr/0054-task-bound-domain-plugins.md
- docs/en/adr/0054-task-bound-domain-plugins.md
- echo-agent-app-core/src/turn_context.rs
- echo-agent-app-core/src/chat_driver.rs
- docs/en/adr/0051-ssh-terminal-sessions.md
- docs/en/adr/0055-project-workspaces.md
- docs/en/architecture/runtime.md
- docs/zh/adr/0051-ssh-terminal-sessions.md
- docs/zh/adr/0055-project-workspaces.md
- docs/zh/architecture/runtime.md
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
- echo-agent-app-core/src/infra/tests.rs
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
- echo-agent-app-core/src/state/tests.rs
- echo-agent-app-core/src/state/workspace.rs
- echo-agent-app-core/src/subagent_loader.rs
- echo-agent-app-core/src/tasks/task_runtime/command_cells.rs
- echo-agent-app-core/src/tasks/task_runtime/completion_gate.rs
- echo-agent-app-core/src/tasks/task_runtime/event_rebuild.rs
- echo-agent-app-core/src/tasks/task_runtime/execution_target.rs
- echo-agent-app-core/src/tasks/task_runtime/executor/dispatch.rs
- echo-agent-app-core/src/tasks/task_runtime/executor/events.rs
- echo-agent-app-core/src/tasks/task_runtime/executor/run.rs
- echo-agent-app-core/src/tasks/task_runtime/executor/tests.rs
- echo-agent-app-core/src/tasks/task_runtime/executor/unattended.rs
- echo-agent-app-core/src/tasks/task_runtime/mod.rs
- echo-agent-app-core/src/tasks/task_runtime/project_environment_tests.rs
- echo-agent-app-core/src/tasks/task_runtime/revisioned_runtime.rs
- echo-agent-app-core/src/tasks/task_runtime/store/runtime.rs
- echo-agent-app-core/src/tasks/task_runtime/store/tests.rs
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
- src-tauri/src/main.rs
- src/tauri/commands/files.rs
- src/tauri/commands/panels.rs
- src/tauri/commands/workspace.rs
- src/tauri/desktop.rs
- src/tauri/mod.rs
- src/tauri/terminal.rs
- web-frontend/src/api/endpoints.memory.test.ts
- web-frontend/src/api/endpoints.ts
- web-frontend/src/components/coding/WorktreePanel.test.tsx
- web-frontend/src/components/coding/WorktreePanel.tsx
- web-frontend/src/components/terminal/Terminal.tsx
- web-frontend/src/components/terminal/TerminalDrawer.test.tsx
- web-frontend/src/components/terminal/TerminalDrawer.tsx
- web-frontend/src/components/workspace/NewTaskDialog.tsx
- web-frontend/src/components/workspace/ProjectSourceFields.test.tsx
- web-frontend/src/components/workspace/ProjectSourceFields.tsx
- web-frontend/src/components/workspace/WorkspaceDialogs.a11y.test.tsx
- web-frontend/src/stores/workspaceStore.ts
supports:
- asset.eko-retained-plugin-runtime
- asset.eko-project-workspaces
- rule.eko-project-location-consistency
limitations:
- Rust/GUI native/real remote and full frontend gates have not run on this candidate.
- Frontend epoch2 covers only its exact 12-source snapshot; later changes require
  delta checks.
- Producer source/static results are not implementation or remote acceptance.
- Old Finding states, the excluded retirement tool repair and dependency-risk decisions
  remain unchanged.
---

# Project workspaces source and focused frontend evidence

## 来源与范围

The clean starting EKO tree is 01137a1e9186fb6c9d9b677e72b3fe4cc0b99a3b. Workspace, host/pool, mounted GUI sources, pre-model consumers, terminal and original project file/worktree adapters are the changed source paths above. The generic provider belongs to the separate Framework checkout; it is not an EKO-specific framework field or a main/pin qualification.

SSH source v1/v2 imports retain their exact producer manifests and previous bytes. GUI file/diff/panels production source3 was imported from candidate154d115762d8142223c27943ce8b35f28c26852347650e26fb51132bb631a91a; its two frontend paths were imported from 13bc67e502828a49b265d15089e013ba1dc6b02e5e530b577253d6565f555f4d and formatted in the single epoch below. The original producer task-source contract1 for missing files/diff mapping is retained; this Asset/Rule provides the real Project authority mapping, not a tool exception or a synthetic zero.

GUI files.rs R1 v2 was subsequently adopted by its exact reviewed delta a008e62fcc6a990068fbea046c6e107cf9203cf7d3842d8488bb86999cb9a377, reaching file SHA8afba4ebd7d10b8b2becd87121b7cd40a83511b6225a06b02fe66e5896f8b495. Execution v2 manifest8db922cb4b2f65c67497875cf350191857783268c56228d6c11c0309a8802cea supplied 23 reviewed source files; all receiver bytes were checked against the untouched base before adoption. Host product-data identity is installed by value, and the obsolete cwd-derived Task product-data helper is removed. These are source review/import facts; their Rust regressions have not run.

## 支持的结论

Epoch2 ran at 2026-10-25T08:53:33.342Z–08:53:46.497Z: five exact Vitest files, 28 passed/0 failed; whole application noEmit TypeScript0; twelve-file ESLint0 with zero warnings; twelve-file Prettier write/check0. The run, raw logs and final 12 file hashes remain in ignored .echo-semantic/local/project-workspaces/node-epoch2. Receipt SHA b47607b4a933fa3ccbeb3144227f2deab5f938761b19bada51e5806d185d486d. The earlier seven-test epoch and its original tsc2/lint1 (unused import) are retained separately and do not substitute for this snapshot. No Vite/browser/native build or SSH was run by this batch. The independent APFS dependency directory was used without npm install, and only its own Vite caches were removed afterwards.

## 已知缺口

### Current combined source boundary

The central source now combines the approved Domain epoch2 85-path contribution with the existing Project source, the independently reviewed SSH v5 import, and immutable GUI checkpoint c403d474. The shared 24 paths were merged against their exact common base, preserving one runtime, binding journal, pool and factory. The existing turn registration now holds prepared primary project values; ordinary run-less chat, TaskRuntime turns and independent primary verifications use the original EKO projector, while fresh default/Plugin instances compose actual checkout rules with the original Domain-only renderer. Added real model-capture, dual-binding replay, boxed task-update and target-artifact regression definitions have not run on this combined source.

The subsequent thin S3 adoption uses the exact frozen final nine-path delivery index f46988b8 and eight-TS manifest e86d5c71. It includes the Domain third argument, S1 late-return fences and S3 fourth submission descriptor, canonical save address and original ConversationStore bootstrap/clear fencing. The incoming entrypoint asset changes only its central source-observation header. Both runtime pages describe the same existing lifecycle; all Rust bytes from source41f764 and independent source-only review374fb1c7 remain unchanged. The producer's 148 tests and 13 behavioral negative assertions belong to its own exact source snapshot. Central TypeScript/build:tauri/native qualification remains unrun, and no producer bundle is substituted for a real central build.

SSHv3's original blocked review is preserved as history; v5 is the approved source successor imported by its own owner, not a claim of remote qualification. ProductDataIo bounded-v2 has an independent source-only PASS (bee037f1); all five Rust definitions still await execution. GUI c403's own 67/21 and static/build results remain producer-snapshot evidence; importing its source does not qualify central frontend or native behavior. The earlier 12-source/28-test epoch is historical. That epoch had only the existing httpdate edge. The actual first Cargo batch subsequently resolved SFTP with workspace-minimal Cargo: actual lock2f1f128b has855 packages, seventeen added identities and one natural removal of itertools0.13.0 in favor of already locked0.14.0. All other838 original identities remain. Exact combined dependency risk is still unaccepted/unqualified; original offline101 and successful online0 are preserved, not replaced by the Domain risk policy.

Direct formatting and source inspection are not Rust compilation. New Workspace/rollback, actual-context/cold Execute/checkpoint, active project-PTY shutdown, transport and GUI file/worktree regressions still require the coordinated Rust batch. Standard SFTP remote CAS capability, real directory/authentication and possible write outcomes require actual target evidence. The manifest dependency has changed but Cargo.lock/risk qualification has not been manufactured. Every future Rust batch must save raw results outside its own target, then clean that exact target after physical readers exit.

The earlier canonical-root preparation candidate is retained in ignored local evidence as a source counterexample, not a failed test result. Fresh Subagents now publish strictly prepared values through the existing pure pre-model projection point after reset/restore; the same real model-capture regression remains unexecuted. The primary's existing EkoContextProjector is preserved. Transport v3 was blocked by independent review and was not imported; later v5 source adoption is distinguished above from actual remote qualification.

The adapter applies to the actual fresh default/Plugin factory paths. It does not establish a concurrency guarantee for arbitrary shared/static fallback Agents. The user's newer Workspace/task navigation meaning supersedes the earlier sidebar interpretation: Workspace means project/engineering workspace, while tasks and conversations belong to it. The earlier twelve-source frontend epoch is retained as historical validation; revised navigation source is adopted from the reviewed c403 checkpoint, with combined/native qualification pending.

The original ProductDataIo supervisor now has an explicit bounded-observation entry, preserving the existing run behavior and original u64 operation owner. Six non-native GUI file IPCs observe for 30 seconds, including time waiting for the original semaphore. Timeout/drop marks the same id retained while its detached physical task and captured host control continue. Only the original completion settles it; late business failures remain in the original failure receipt. Four controlled regression definitions cover host-retirement blocking, observer loss, independent ids and known/late errors. Direct formatting is the only engineering check run for this increment: these new regressions have not run. This does not close the SSH transport's early-unknown-return findings.

The v1 independent source review blocked R1: a successful buffered send was incorrectly treated as observation, so cancellation before receiver consumption could lose an error. That original review and source remain preserved. The v2 source transfers the same original operation into the completed-result envelope, acknowledges on actual consumption, and records an unobserved buffered error on envelope drop. A fifth controlled regression explicitly waits for successful buffering, leaves the observer unpolled, then drops it and verifies failure retention and another id's independence. The original observed-error positive control remains. All five definitions are still uncompiled/unrun; incremental source review passed, which does not establish runtime or remote qualification.

Formal EKO documentation is in ADR0055 and both runtime architecture pages. GUI/IPC-specific behavior does not require a new SDK example; the Framework provider's example is qualified by its own compilation chain. Website synchronization follows a real clean EKO checkpoint and its original sync mechanism; no deployment or release is claimed.


## 2026-10-29：退役后的前置路径导航维护

本节只维护结构化源码/消费者引用的导航，不重新观察或续期旧运行资格。原观察 `source:f49ec6694fa4b4cf0f175f3a746b2a82a610bfd044d86e11e72f88763aa54c7c`、上文历史原件、失败与限制、原 command/scenario/after revision 均保持原范围；本节不新增 matched、状态或 owner。

| 原历史路径 | 当前实现或退役决定导航 |
| --- | --- |
| `src/main.rs` | `src-tauri/src/main.rs` |

旧路径身份与退出范围由既有退役资产/Finding和正式ADR0050/0057保留；源码历史可在 `pre-retirement-2026-10-24` 及后继具名Git保存点回读。新导航只指向现存原接管实现或退出决定，不能把旧observed_at解释成这些路径当时已经运行通过。unknown/needs_review、原动态/native/风险缺口不清；本候选没有执行Cargo或官方新Task校验，采用与真实来源绑定由EKO唯一writer按正常新Task合同完成。
