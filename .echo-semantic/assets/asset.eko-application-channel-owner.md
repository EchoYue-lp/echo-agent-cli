---
schema_version: 1
id: asset.eko-application-channel-owner
kind: asset
title: Original EKO application channel owner
asset_type: state_authority
status: active
risk: high
observed_at: source:4726a37cc1350e66b30b281fd02e16cdd087cd7f44b58b28a0ba6ab09bfa0d46
boundary_refs: [boundary.eko-framework-consumer-settlement]
behavior_refs: []
rule_refs: [rule.eko-framework-consumer-authorities]
evidence_refs: [evidence.eko-channel-helper-relocation]
finding_refs: [finding.eko-channel-helper-relocation]
candidate_refs: []
code_refs: [echo-agent-app-core/src/channel_runtime.rs, echo-agent-app-core/src/channel_runtime/input_pump.rs, echo-agent-app-core/src/channel_runtime/outbound.rs, echo-agent-app-core/src/channel_runtime/tool_render.rs, echo-agent-app-core/src/channel_runtime/management.rs]
consumer_refs: [src/cli/channels.rs, echo-agent-app-core/src/runtime.rs, src/tauri/state.rs]
---

# Original EKO application channel owner

## 资产身份

The same original ChannelManager, session/coordinator/pump and text adapters now live in app-core. ApplicationServices and Tauri share its original Arc; no new authority, store, queue or executor replaces it.

## 来源与消费者

Original module and feature forwarding select one implementation. Thin retained bin entry points and GUI consume the same application-side owner.

## 生命周期

The original admission, incarnation, task handles and settlement responsibilities remain authoritative. This source asset records placement, not executed runtime acceptance.

## 候选关系

finding.eko-channel-helper-relocation migrates only three old helper locations to asset.eko-application-channel-owner. No additional runtime authority is created by this metadata.

## 未知与限制

Mechanical relocation has independent source-only review. The combined source has no Cargo/Clippy/Rust/native or real IM qualification; complete Plan07 and dependency risk gates remain pending.
