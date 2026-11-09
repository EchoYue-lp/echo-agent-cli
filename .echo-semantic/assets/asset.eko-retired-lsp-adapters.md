---
schema_version: 1
id: asset.eko-retired-lsp-adapters
kind: asset
title: EKO retired LSP adapters
asset_type: entrypoint
status: deprecated
observed_at: db5010ed888cf50f2400fbba31075ab63c5626e9
risk: high
boundary_refs: [boundary.eko-framework-consumer-settlement]
behavior_refs: []
rule_refs: [rule.eko-framework-consumer-authorities]
evidence_refs: []
finding_refs: [finding.eko-lsp-consumer-retirement]
candidate_refs: []
code_refs: [src/tauri/commands/lsp.rs, web-frontend/src/components/lsp/LspPanel.tsx, web-frontend/src/generated/LspCommand.ts]
consumer_refs: [src/tauri/commands/mod.rs, src/tauri/mod.rs, web-frontend/src/components/layout/SettingsDialog.tsx, web-frontend/src/generated/ExtensionCommand.ts, web-frontend/src/generated/ExtensionCommandRequest.ts, web-frontend/src/generated/index.ts]
---

# EKO retired LSP adapters

## 资产身份

Historical dedicated LSP IPC/controller/panel/wire files at the task base, explicitly retired by Plan04 and ADR 0050.

## 来源与消费者

Tauri module/invoke, Settings loader and generated command/barrel consumers are withdrawn before file deletion. The old source refs remain bound to the historical task-base Git revision.

## 生命周期

No controller, language-server manager or replacement executor is retained. This asset does not own user configuration files.

## 候选关系

The dedicated files retire; asset.eko-retained-plugin-runtime remains the canonical owner of all legal plugin and extension behavior.

## 未知与限制

The narrow delete checkpoint does not close framework diagnostic API bridging, full Rust/GUI matrices or Plan04 as a whole. No user configuration file is deleted.
