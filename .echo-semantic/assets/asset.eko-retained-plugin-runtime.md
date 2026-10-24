---
schema_version: 1
id: asset.eko-retained-plugin-runtime
kind: asset
title: EKO retained plugin and extension authorities
asset_type: state_authority
status: active
observed_at: source:df929b9f26f9756c1885492dca0d1f7be6e3d6ed8e61ddfdc268331c3bb71d60
risk: high
boundary_refs: [boundary.eko-framework-consumer-settlement]
behavior_refs: []
rule_refs: [rule.eko-framework-consumer-authorities]
evidence_refs: []
finding_refs: [finding.eko-lsp-consumer-retirement]
candidate_refs: []
code_refs: [echo-agent-app-core/src/plugin_runtime/runtime.rs, echo-agent-app-core/src/plugin_runtime/publication.rs, echo-agent-app-core/src/plugin_runtime/types.rs, echo-agent-app-core/src/extension_commands.rs, echo-agent-app-core/src/extension_control/service.rs, echo-agent-app-core/src/config_discovery.rs]
consumer_refs: [echo-agent-app-core/src/runtime.rs, echo-agent-app-core/src/agent_pool/pool.rs, src/tauri/commands/mod.rs, src/tauri/mod.rs, web-frontend/src/components/plugins/PluginPanel.tsx, web-frontend/src/api/endpoints.ts]
---

# Retained plugin and extension authorities

## 资产身份

Existing EKO plugin-generation and extension-dispatch state authorities. No replacement executor is introduced.

## 来源与消费者

Bootstrap, AgentPool and retained Tauri/frontend projections consume the original runtime, publication and extension services listed above.

## 生命周期

These original owners retain generation publication, MCP claims, root changes, rollback and cleanup receipts. Existing lifecycle tests remain and pass in the focused batch.

## 候选关系

Canonical retained owner for the narrow dedicated-adapter retirement Finding. Retired-input diagnostic wording remains EKO application policy.

## 未知与限制

This asset does not claim full framework-consumer verification. The generic diagnostics increment removes the retired public-API dependency and qualifies the rebound exact-six risk policy. Full matrices and main CI are pending; the broad Rule remains needs_review.
