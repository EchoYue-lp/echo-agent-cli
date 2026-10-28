---
schema_version: 1
id: asset.eko-retained-plugin-runtime
kind: asset
title: EKO retained plugin and extension authorities
asset_type: state_authority
status: active
observed_at: source:f49ec6694fa4b4cf0f175f3a746b2a82a610bfd044d86e11e72f88763aa54c7c
risk: high
boundary_refs: [boundary.eko-framework-consumer-settlement]
behavior_refs: []
rule_refs: [rule.eko-framework-consumer-authorities]
evidence_refs: [evidence.eko-project-workspaces-source]
finding_refs: [finding.eko-lsp-consumer-retirement]
candidate_refs: []
code_refs: [echo-agent-app-core/src/plugin_runtime/runtime.rs, echo-agent-app-core/src/plugin_runtime/publication.rs, echo-agent-app-core/src/plugin_runtime/types.rs, echo-agent-app-core/src/extension_commands.rs, echo-agent-app-core/src/extension_control/service.rs, echo-agent-app-core/src/config_discovery.rs, echo-agent-app-core/src/domain_plugin.rs, echo-agent-app-core/src/subagent_prompt.rs, echo-agent-app-core/src/tasks/task_runtime/profiles.rs, web-frontend/src/generated/DomainBindingRef.ts, web-frontend/src/generated/DomainPluginIdentity.ts, web-frontend/src/generated/DomainRoleBinding.ts]
consumer_refs: [src/tauri/commands/plugins.rs, src/tauri/commands/extensions.rs, echo-agent-app-core/src/runtime.rs, echo-agent-app-core/src/agent_pool/pool.rs, src/tauri/commands/mod.rs, src/tauri/mod.rs, web-frontend/src/components/plugins/PluginPanel.tsx, web-frontend/src/api/endpoints.ts, echo-agent-app-core/src/plugin_components.rs, echo-agent-app-core/src/tasks/task_runtime/types.rs, echo-agent-app-core/src/tasks/task_runtime/store/tests.rs, docs/en/adr/0054-task-bound-domain-plugins.md, docs/zh/adr/0054-task-bound-domain-plugins.md, examples/domain-plugin/README.md]
---

# Retained plugin and extension authorities

## 资产身份

Existing EKO plugin-generation and extension-dispatch state authorities. No replacement executor is introduced.

## 来源与消费者

Bootstrap, AgentPool and retained Tauri/frontend projections consume the original runtime, publication and extension services listed above.

The original prepare/publication path also owns EKO domain declarations and the prepared role catalog. `domain_plugin.rs` reads those frozen inputs and derives a fixed binding; original RunCreated, task extension, pool admission and completion consumers carry it without a second registry or task graph. `profiles.rs` and the existing prompt compiler project the selected roles; the three generated identity DTOs are the same source-declared fields crossing the existing GUI contract. ADR0054 and the existing package example describe this source boundary.

## 生命周期

These original owners retain generation publication, MCP claims, root changes, rollback and cleanup receipts. Existing lifecycle tests remain and pass in the focused batch.

That focused result belongs to its original snapshot. The current Project+Domain source union preserves both location and method identities, their original physical admission, and the same prepare/publication owner. Combined Rust/type/native/remote qualification is still pending; extending this mapping does not inherit the older focused result.

## 候选关系

Canonical retained owner for the narrow dedicated-adapter retirement Finding. Retired-input diagnostic wording remains EKO application policy.

## 未知与限制

This asset does not claim full framework-consumer verification. The generic diagnostics increment removes the retired public-API dependency and qualifies the rebound exact-six risk policy. Full matrices and main CI are pending; the broad Rule remains needs_review.


## 2026-10-29：退役后的前置路径导航维护

本节只维护结构化源码/消费者引用的导航，不重新观察或续期旧运行资格。原观察 `source:f49ec6694fa4b4cf0f175f3a746b2a82a610bfd044d86e11e72f88763aa54c7c`、上文历史原件、失败与限制、原 command/scenario/after revision 均保持原范围；本节不新增 matched、状态或 owner。

| 原历史路径 | 当前实现或退役决定导航 |
| --- | --- |
| `src/cli/cmd_impls/coding.rs` | `src/tauri/commands/plugins.rs` |
| `src/cli/repl.rs` | `src/tauri/commands/extensions.rs` |

旧路径身份与退出范围由既有退役资产/Finding和正式ADR0050/0057保留；源码历史可在 `pre-retirement-2026-10-24` 及后继具名Git保存点回读。新导航只指向现存原接管实现或退出决定，不能把旧observed_at解释成这些路径当时已经运行通过。unknown/needs_review、原动态/native/风险缺口不清；本候选没有执行Cargo或官方新Task校验，采用与真实来源绑定由EKO唯一writer按正常新Task合同完成。
