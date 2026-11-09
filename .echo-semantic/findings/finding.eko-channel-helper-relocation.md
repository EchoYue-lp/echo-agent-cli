---
schema_version: 1
id: finding.eko-channel-helper-relocation
kind: finding
type: consolidation_candidate
status: resolved
severity: medium
primary_focus: contract_evidence
focus: [state_authority, time_lifecycle]
boundary_ref: boundary.eko-framework-consumer-settlement
behavior_refs: []
rule_refs: [rule.eko-framework-consumer-authorities]
evidence_refs: [evidence.eko-channel-helper-relocation]
audit_refs: [audit.eko-channel-helper-relocation]
decision_refs: [docs/en/adr/0053-im-cron-gui-control.md]
repair_evidence_refs: [evidence.eko-channel-helper-relocation]
verification_evidence_refs: [evidence.eko-channel-helper-relocation]
rereview_audit_refs: [audit.eko-channel-helper-relocation]
discovered_at: 28bb9459bde9872b8d36cdd4a6eb98929feb0927
decision: migrate
candidate_asset_refs: [asset.eko-bin-channel-helpers, asset.eko-application-channel-owner]
canonical_asset_ref: asset.eko-application-channel-owner
replacement_refs: [asset.eko-application-channel-owner]
delete_paths: [src/cli/channels/input_pump.rs, src/cli/channels/outbound.rs, src/cli/channels/tool_render.rs]
rollback_ref: 28bb9459bde9872b8d36cdd4a6eb98929feb0927
---

# Obsolete bin helper locations after original-owner relocation

## 问题

Keeping original channel helpers owned by the bin prevents app-core/GUI from consuming the same owner. Plan07 requires relocating the original implementation, switching real callers and keeping bin entry points thin.

## 触发条件与影响

Only src/cli/channels/input_pump.rs, outbound.rs and tool_render.rs are removed from their old locations. Each corresponding implementation remains under echo-agent-app-core/src/channel_runtime/. No original behavior is deleted and no second manager, session owner or queue replaces it.

## 证据

Evidence and the independent source-only audit compare the three old HEAD28bb files with the new files and verify original module/feature/caller switches. The narrow relocation review passed. Runtime compilation and behavior acceptance remain unexecuted and unclaimed.

## 处理记录

Resolved means the three obsolete source locations have been replaced and independently reviewed as a source relocation. It does not resolve the entire Plan07, qualify new runtime paths, renew dependency acceptance or claim native Hook/main acceptance. Recovery restores the exact HEAD28bb paths in a reviewed change without overwriting later work.
