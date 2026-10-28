---
schema_version: 1
id: rule.eko-project-location-consistency
kind: rule
status: needs_review
expectation: human_confirmed
risk: high
primary_focus: state_authority
focus:
- state_authority
- data_durability
- time_lifecycle
- failure_concurrency
- contract_evidence
observed_at: source:f49ec6694fa4b4cf0f175f3a746b2a82a610bfd044d86e11e72f88763aa54c7c
behavior_refs: []
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
- src-tauri/src/main.rs
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
evidence_refs:
- evidence.eko-project-workspaces-source
finding_refs: []
---

# Project location consistency

## 不变量或唯一权威

原 Workspace manifest/registry 同时拥有用户位置、经过验证的实际执行位置和原 project_root_revision；原 host 保持产品数据在本机。每次文件、命令、上下文、隔离与证据读取使用对应 host 和 invocation cwd，不把本机同名路径当远端。SSH alias 配置变化或目录解析变化不能绕过已冻结的实际身份。

## 适用行为

GUI 选择已有本地文件夹、新建本地文件夹或 SSH 项目；重开、切换、重连；项目树/编辑器/diff/changes、Git/worktree、Agent/Subagent 模型前指令和执行、验收产物及项目终端。用户级配置/记忆/产品 journal 与独立本地终端保持其本机权威。

## 当前实现

当前任务分支已落相应源码和正式 ADR；独立生产者仍按精确源码切片合流。位置相关 TaskRuntime/GUI/SSH 合格性必须由同输入实际验证完成，不能用 getter/配置字段存在代替调用。前端 epoch2 的 28 项和静态检查已实际通过；当前没有本轮 Rust、真实远端或原生 GUI 通过结论。

## 期望行为

同原 owner 准入与结算；必要读取错误在原可失败的模型入口阻止执行，不能只在 projector 中警告。本地既有文件和失败创建时非自有内容保留。重连核原有效身份，删除/重关联等待真实资源空闲，原 PTY child/reader 与 shared SSH master 按依赖结算。CAS、隔离或资源能力不能履行时给真实诊断，不降低合同、不虚构完成或重放未知写。

## 证据

Evidence 绑定本轮源码和前端精确 epoch；原无映射的 files.rs/diff.rs 由该具名资产关联，不修改外部工具、不引用历史 SSH terminal Evidence 充当项目资格。

## 裁决记录

用户最新确认 Workspace＝项目＝工程，任务和会话归属 Workspace；创建 Workspace 选择本地已有/新建/远程三来源，新建任务不创建目录。原前端 epoch2 保留为历史输入，尚不覆盖新导航语义。Project-workspaces 的位置/未知结算义务继续有效，ADR0055 是本仓长期决定。此 Rule 仍 needs_review，未关闭历史 Finding、未接受新的 lock 风险，未宣称 main/CI/release。


## 2026-10-29：退役后的前置路径导航维护

本节只维护结构化源码/消费者引用的导航，不重新观察或续期旧运行资格。原观察 `source:f49ec6694fa4b4cf0f175f3a746b2a82a610bfd044d86e11e72f88763aa54c7c`、上文历史原件、失败与限制、原 command/scenario/after revision 均保持原范围；本节不新增 matched、状态或 owner。

| 原历史路径 | 当前实现或退役决定导航 |
| --- | --- |
| `src/main.rs` | `src-tauri/src/main.rs` |

旧路径身份与退出范围由既有退役资产/Finding和正式ADR0050/0057保留；源码历史可在 `pre-retirement-2026-10-24` 及后继具名Git保存点回读。新导航只指向现存原接管实现或退出决定，不能把旧observed_at解释成这些路径当时已经运行通过。unknown/needs_review、原动态/native/风险缺口不清；本候选没有执行Cargo或官方新Task校验，采用与真实来源绑定由EKO唯一writer按正常新Task合同完成。
