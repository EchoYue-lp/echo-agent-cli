---
schema_version: 1
id: audit.eko-lsp-consumer-retirement
kind: audit
boundary_ref: boundary.eko-framework-consumer-settlement
lens: contract_evidence
freshness: examined
revision: source:df929b9f26f9756c1885492dca0d1f7be6e3d6ed8e61ddfdc268331c3bb71d60
finding_refs: [finding.eko-lsp-consumer-retirement]
challenges:
  deleted-adapters-still-have-live-callers:
    source_refs: [src/tauri/commands/mod.rs, src/tauri/mod.rs, web-frontend/src/components/layout/SettingsDialog.tsx, web-frontend/src/generated/ExtensionCommand.ts, web-frontend/src/generated/ExtensionCommandRequest.ts, web-frontend/src/generated/index.ts]
    evidence_refs: [evidence.eko-lsp-consumer-retirement]
  retirement-removes-shared-components-or-imports:
    source_refs: [echo-agent-app-core/src/plugin_runtime/tests.rs, echo-agent-app-core/src/extension_control/tests.rs, echo-agent-app-core/src/extension_commands.rs, web-frontend/src/generated/ExtensionCommandReceipt.ts]
    evidence_refs: [evidence.eko-lsp-consumer-retirement]
  frozen-diagnostic-loss-or-generation-drift:
    source_refs: [echo-agent-app-core/src/plugin_runtime/types.rs, echo-agent-app-core/src/plugin_runtime/tests.rs, echo-agent-app-core/src/plugin_runtime/publication.rs]
    evidence_refs: [evidence.eko-lsp-consumer-retirement]
  exact-risk-fingerprint-renewal-expands-acceptance:
    source_refs: [.cargo/accepted-dependency-risks.json, docs/supreme/specs/cancellation-observation-consumer/dependency-risk-decision.md, scripts/check_cli_dependency_risks.py, scripts/test_cli_dependency_risks.py]
    evidence_refs: [evidence.eko-lsp-consumer-retirement]
---

# Narrow deletion checkpoint review

## 审查范围

Independent read-only review of current caller/module/invoke withdrawal, five-type generated diff and the three-file dedicated-adapter deletion checkpoint. This is not a whole-Outcome qualification review.

## 已检查故障假设

Dedicated adapters still have production callers; generated cleanup removes Browser/shared imports; plugin retirement deletes legal components or bypasses the existing receipt owners.

## 实际实现路径与证据

The independent reviewer checked the actual raw export hashes, 131 passing focused tests and feature graph: narrow checkpoint pass, zero action items. The main agent additionally built an exact ignored frontend deletion candidate, then removed only the three named files under normal repair/deletePaths preflight. Actual frontend build/test passed (58 files, 295 tests), and app-core source hashes remain identical to the focused Rust batch.

## 问题记录

Earlier review fixes for visible validation diagnostics and frozen-generation observation have direct regression proof. Deletion-preparation and completed post-delete increment reviews both returned pass with zero action items. The official task-scope checker passed after binding Evidence to its actual task source signature.

## 残余风险

The direct framework LSP-specific dependency has now been removed through the separately reviewed generic-diagnostics increment. The broad Rule remains needs_review; no inventory/model closure state changes.

## 未检查项

Full Rust/GUI/example matrices and exact main CI remain separate pending qualification. The new source/risk increment has independent review and current 133-test/Clippy/fmt/type/risk proof; final contribution-level independent review returned pass with zero action items over all57paths. Final staged task/asset verification remains the next deterministic step. No additional Cargo or native product acceptance is claimed.
