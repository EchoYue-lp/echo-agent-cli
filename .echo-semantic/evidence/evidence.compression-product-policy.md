---
schema_version: 1
id: evidence.compression-product-policy
kind: evidence
observed_at: source:621b3930bfef37174da8309c6fde5c06dd749fd2be9cb4d48fdaf1e8c63628b1
source_refs: [echo-agent-app-core/src/config.rs, echo-agent-app-core/src/manual_compression.rs, echo-agent-app-core/src/tasks/task_runtime/compact_context.rs, src/cli/cmd_impls/context.rs, src/cli/channels.rs, src/tui/commands.rs, src/tui/events.rs, src/tauri/commands/panels.rs, web-frontend/src/api/endpoints.ts, web-frontend/src/types/api.ts, web-frontend/src/components/compress/CompressPanel.tsx, config/eko.example.yaml, docs/zh/configuration.md, docs/en/configuration.md, docs/zh/features.md, docs/en/features.md, docs/zh/architecture/persistence.md, docs/en/architecture/persistence.md, docs/zh/adr/0048-context-compression-product-policy.md, docs/en/adr/0048-context-compression-product-policy.md]
supports: [asset.eko-conversation-authorities, rule.eko-framework-consumer-authorities]
limitations: [Local candidate is not merged; depends on framework compression branch; native UI and external provider acceptance are not established]
---

# EKO compression product policy

## 支持的结论

EKO defaults to compress_window=0, selecting the framework's live-window recent
token allowance. Explicit positive windows retain legacy message caps.
CLI/TUI/GUI/channel share the installed policy, focus, framework provider
cancellation and one app-core journal safe point. The ineffective keep_messages
request field is removed. GUI/TUI can supply focus; receipts retain checkpoint
strategy, protected count and before/after token accounting.

TaskRuntimeStore remains goal/steer/recovery authority. The latest four bounded
steer excerpts keep their ending restriction and chronological correction order,
without a second 200-character projection cut. Goal/capsule refresh is replaceable
and scope removal retains its previous behavior.

## 来源与范围

The compression-focused app-core tests passed for four surface focus/journal
routes, pre-transform cancellation, post-transform cancellation safe points,
caller drop, typed failure and repeated real projection compression. The frontend
suite passed 287 tests and built. The ten focused app-core tests include an owned
cancellation terminal on every surface. Independent review returned pass with
zero remaining findings; full lint and panic-API checks passed. Final workspace
and GUI checks still bind the final candidate rather than implying remote delivery.

All applicable local final checks passed: the all-feature workspace recorded
1,922 passed and zero failed (15 marked doctests ignored), app-core without
default features compiled, and the separate GUI check/test completed after
clearing debug caches and aligning C/Rust to macOS 11.0. Final receipts:
`eko-outcome-final-1791559251408.log` (both lint gates),
`eko-final-resumed-1791559992553.log` (workspace/no-default check; its final GUI
link exhausted disk), `eko-gui-final-resourced-1791562837600.log` (GUI rerun),
and `eko-frontend-outcome-final-1791559251408.log` (format/lint/test/build).
The failed environmental attempt is retained, not treated as a passing GUI gate.

## 已知缺口

Framework and application branches are local candidates; native UI and real provider acceptance are not established.
