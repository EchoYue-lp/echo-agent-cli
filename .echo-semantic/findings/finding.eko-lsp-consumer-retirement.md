---
schema_version: 1
id: finding.eko-lsp-consumer-retirement
kind: finding
type: consolidation_candidate
status: resolved
severity: high
primary_focus: contract_evidence
focus: [state_authority, time_lifecycle]
boundary_ref: boundary.eko-framework-consumer-settlement
behavior_refs: []
rule_refs: [rule.eko-framework-consumer-authorities]
evidence_refs: [evidence.eko-lsp-consumer-retirement]
audit_refs: [audit.eko-lsp-consumer-retirement]
decision_refs: [docs/en/adr/0050-gui-product-convergence.md]
repair_evidence_refs: [evidence.eko-lsp-consumer-retirement]
verification_evidence_refs: [evidence.eko-lsp-consumer-retirement]
rereview_audit_refs: [audit.eko-lsp-consumer-retirement]
discovered_at: db5010ed888cf50f2400fbba31075ab63c5626e9
decision: retire
candidate_asset_refs: [asset.eko-retired-lsp-adapters, asset.eko-retained-plugin-runtime]
canonical_asset_ref: asset.eko-retained-plugin-runtime
replacement_refs: [asset.eko-retained-plugin-runtime]
delete_paths: [src/tauri/commands/lsp.rs, web-frontend/src/components/lsp/LspPanel.tsx, web-frontend/src/generated/LspCommand.ts]
rollback_ref: db5010ed888cf50f2400fbba31075ab63c5626e9
---

# Retire EKO dedicated LSP adapters

## 问题

Exact Plan04 and ADR 0050 authorize EKO A2A/LSP consumer withdrawal. The framework retains production APIs in this outcome. Removal is limited to the three named dedicated files after the current callers and generated references are removed.

## 触发条件与影响

First disconnect bootstrap/scoped tool registration, LSP plugin branches, module/invoke registration and Settings loader. Run focused app-core plugin lifecycle/legacy diagnostic/extension tests; export affected contracts with ts-rs to an isolated directory. Then independently review the frozen caller switches and delete only the named files through explicit deletePaths. Run the complete frontend build after physical deletion, and review the final increment.

## 处理记录

The narrow caller-switch/replacement, focused verification and deletion-preparation independent review checkpoint is resolved: 131 tests pass, real types are merged, and the exact frontend deletion candidate builds. This supplies the required approved repair reference for executing the three named deletes. All three files were subsequently removed under the recorded repair reference and exact deletePaths. The real frontend matches the tested candidate hashes; build and 58-file/295-test validation passed. The independent completed-deletion increment review returned pass with zero action items and is recorded in Evidence/Audit. This Finding settles only the dedicated adapter deletion. The generic diagnostics increment is separately qualified in the same Evidence; full Plan04 qualification remains pending.

## 证据

The narrow independent review and 131 focused tests are recorded in evidence.eko-lsp-consumer-retirement and audit.eko-lsp-consumer-retirement. Actual post-delete frontend build and 295 tests pass. The completed deletion increment received independent pass; the separately reviewed generic diagnostics increment now removes that framework API dependency.

## Recovery

Restore this task's files from the exact base using a separate, reviewed recovery change. Do not overwrite later unrelated edits. No user plugin/config files are removed or rewritten. Full product recovery anchor remains pre-retirement-2026-10-24, independently owned by the coordinator.
