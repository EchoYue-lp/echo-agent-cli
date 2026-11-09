---
schema_version: 1
id: asset.eko-bin-channel-helpers
kind: asset
title: Original bin-owned channel helper locations
asset_type: entrypoint
status: deprecated
risk: high
observed_at: 28bb9459bde9872b8d36cdd4a6eb98929feb0927
boundary_refs: [boundary.eko-framework-consumer-settlement]
behavior_refs: []
rule_refs: [rule.eko-framework-consumer-authorities]
evidence_refs: [evidence.eko-channel-helper-relocation]
finding_refs: [finding.eko-channel-helper-relocation]
candidate_refs: []
code_refs: [src/cli/channels/input_pump.rs, src/cli/channels/outbound.rs, src/cli/channels/tool_render.rs]
consumer_refs: [src/cli/channels.rs, echo-agent-app-core/src/runtime.rs, src/tauri/state.rs]
---

# Original bin-owned channel helper locations

## 资产身份

Historical bin-owned locations at HEAD28bb. The implementation moves rather than being removed; current callers forward to the canonical original app-core owner.

## 来源与消费者

Original module and feature forwarding select one implementation. Thin retained bin entry points and GUI consume the same application-side owner.

## 生命周期

The original admission, incarnation, task handles and settlement responsibilities remain authoritative. This source asset records placement, not executed runtime acceptance.

## 候选关系

finding.eko-channel-helper-relocation migrates only three old helper locations to asset.eko-application-channel-owner. No additional runtime authority is created by this metadata.

## 未知与限制

Mechanical relocation has independent source-only review. The combined source has no Cargo/Clippy/Rust/native or real IM qualification; complete Plan07 and dependency risk gates remain pending.
