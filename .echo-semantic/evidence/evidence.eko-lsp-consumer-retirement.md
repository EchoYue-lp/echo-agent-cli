---
schema_version: 1
id: evidence.eko-lsp-consumer-retirement
kind: evidence
observed_at: source:df929b9f26f9756c1885492dca0d1f7be6e3d6ed8e61ddfdc268331c3bb71d60
source_refs: [.cargo/accepted-dependency-risks.json, docs/supreme/specs/cancellation-observation-consumer/dependency-risk-decision.md, scripts/check_cli_dependency_risks.py, scripts/test_cli_dependency_risks.py, docs/en/adr/0050-gui-product-convergence.md, echo-agent-app-core/src/plugin_runtime/tests.rs, echo-agent-app-core/src/plugin_runtime/runtime.rs, echo-agent-app-core/src/plugin_runtime/publication.rs, echo-agent-app-core/src/extension_commands.rs, echo-agent-app-core/src/config_discovery.rs, echo-agent-app-core/src/config_watcher.rs, echo-agent-app-core/src/developer_commands.rs, echo-agent-app-core/src/extension_control/tests.rs, src/tauri/commands/mod.rs, src/tauri/mod.rs, web-frontend/src/components/layout/SettingsDialog.tsx, web-frontend/src/components/layout/SettingsDialog.test.tsx, web-frontend/src/components/plugins/PluginPanel.test.tsx, web-frontend/src/generated/ExtensionCommand.ts, web-frontend/src/generated/ExtensionCommandRequest.ts, web-frontend/src/generated/ExtensionCommandReceipt.ts, web-frontend/src/generated/ExtensionKind.ts, web-frontend/src/generated/PluginReloadProjection.ts, web-frontend/src/generated/index.ts]
supports: [rule.eko-framework-consumer-authorities]
limitations: [Only the three dedicated adapter file retirement checkpoint is qualified, The generic diagnostics increment is locally qualified but full Plan04 and main acceptance remain pending, Full workspace and GUI matrices and exact main CI not run, Local raw evidence is available only in this worktree]
evidence_type: behavior_equivalence
before_revision: db5010ed888cf50f2400fbba31075ab63c5626e9
after_revision: task:0cdab95484c4a838e33b1190bb1be05e3bfbbef1905681d35a3210b964d296a1
deleted_paths: [src/tauri/commands/lsp.rs, web-frontend/src/components/lsp/LspPanel.tsx, web-frontend/src/generated/LspCommand.ts]
coverage: [retained plugin publication and cleanup authorities, legal mixed components and legacy diagnostics, remaining extension contracts, Settings loader withdrawal and obsolete-tab fallback, intentional dedicated adapter retirement]
scenario_results:
  retained-plugin-components-and-cleanup:
    status: matched
    source_refs: [echo-agent-app-core/src/plugin_runtime/tests.rs, echo-agent-app-core/src/plugin_runtime/runtime.rs, echo-agent-app-core/src/plugin_runtime/publication.rs]
  retained-extension-and-config-owners:
    status: matched
    source_refs: [echo-agent-app-core/src/extension_control/tests.rs, echo-agent-app-core/src/config_watcher.rs, echo-agent-app-core/src/extension_commands.rs]
  retired-settings-and-generated-callers:
    status: matched
    source_refs: [web-frontend/src/components/layout/SettingsDialog.test.tsx, web-frontend/src/generated/ExtensionCommand.ts, web-frontend/src/generated/ExtensionCommandRequest.ts, web-frontend/src/generated/index.ts, src/tauri/commands/mod.rs, src/tauri/mod.rs]
command_results:
  - { command: 'cargo check -p echo-agent-app-core --locked', exit_code: 0 }
  - { command: 'cargo test -p echo-agent-app-core --lib plugin_runtime:: --locked', exit_code: 0 }
  - { command: 'cargo test -p echo-agent-app-core --lib extension_commands:: --locked', exit_code: 0 }
  - { command: 'cargo test -p echo-agent-app-core --lib config_discovery:: --locked', exit_code: 0 }
  - { command: 'cargo test -p echo-agent-app-core --lib config_watcher:: --locked', exit_code: 0 }
  - { command: 'cargo test -p echo-agent-app-core --lib developer_commands:: --locked', exit_code: 0 }
  - { command: 'cargo test -p echo-agent-app-core --lib extension_control::tests:: --locked', exit_code: 0 }
  - { command: 'pre-deletion frontend candidate: npm run build', exit_code: 0 }
  - { command: 'post-deletion real web-frontend directory: npm run build', exit_code: 0 }
  - { command: 'post-deletion real web-frontend directory: npm test', exit_code: 0 }
  - { command: 'current e4e bridge: cargo clippy -p echo-agent-app-core --lib --locked -- -D warnings -D clippy::unwrap_used -D clippy::expect_used -D clippy::panic -D clippy::unreachable', exit_code: 0 }
  - { command: '/opt/anaconda3/bin/python3 -B scripts/test_cli_dependency_risks.py', exit_code: 0 }
  - { command: '/opt/anaconda3/bin/python3 -B scripts/check_cli_dependency_risks.py --report-prefix .echo-semantic/local/eko-lsp-consumer-decoupling/e4e-risk-actual-ssh (process-only HTTP1.1/exact official SSH URL rewrite)', exit_code: 0 }
  - { command: '/opt/anaconda3/bin/python3 -B scripts/check_cli_dependency_risks.py --registry-origins --report-prefix .echo-semantic/local/eko-lsp-consumer-decoupling/e4e-risk-origin-ssh (process-only HTTP1.1/exact official SSH URL rewrite)', exit_code: 0 }
---

# Dedicated LSP adapter retirement checkpoint

## 支持的结论

The equivalence claims cover retained components, publication/root/receipt owners and remaining extension dispatch. LSP execution and its dedicated GUI/IPC/wire adapters are intentionally retired by Plan04 and ADR 0050; they are not claimed equivalent. This original checkpoint does not settle the wider framework-consumer Rule or Plan04. The completed generic-diagnostics increment is recorded below.

## 来源与范围

Initial accepted c3b checkpoint: app-core check passed without warnings. Focused tests passed 46 plugin, 47 extension (including real ts-rs export tests), 6 legacy config, 6 watcher, 4 developer and 22 extension-owner tests, 131 total with no failures/ignored tests/warnings. Existing root publication, cleanup receipt and watcher ABA regressions remain. Actual dependency feature output contains no lsp/a2a feature.

Real ts-rs exported 47 files into an isolated directory; only five expected types were copied. Their formatted diff removes only LSP arms/import/count; remaining imports and Browser contracts remain. Barrel export was removed separately. No generated success stub was written.

Before physical deletion, a real frontend candidate build passed. The ignored candidate excludes only the two approved dedicated frontend paths, copies the other 421 files with recorded hashes, and uses separate build-cache files. The production files remain until authorized deletePaths are recorded. This is candidate proof, not a claim that actual deletion or the production-directory build has already occurred.

## Review and retained originals

Independent reviewer returned narrow deletion-checkpoint pass with zero action items after checking current callers, actual raw export hashes, 131 test results and feature graph. Raw logs, focused-run.json, generated-merge.json, raw ts-rs outputs and candidate hashes remain under ignored .echo-semantic/local/eko-lsp-consumer-decoupling/. They are local originals, not checked-in release evidence. lsof found no target readers; cargo clean removed 5.4GiB and target is absent.

## 已知缺口

All three authorized files were physically removed after normal high preflight recording with the exact repair reference/deletePaths. The actual frontend source matches all 421 candidate input hashes, with only the approved dedicated files absent. Real-directory npm run build and npm test both exit 0: 58 files, 295 tests. Prettier, bilingual parity (61 pairs/51 ADRs), git diff --check pass, and the dedicated controller/panel/type/count identifier search has no remaining source hits. The app-core/manifests/lock input hashes still match the 131-test batch exactly. No further Cargo was started and target remains absent.

The completed deletion increment received independent read-only pass with zero action items. The reviewer confirmed the 421 frontend and 204 Rust/manifest/lock input bindings, actual build/295-test result, narrow generated diff and preserved shared owners. The generic-diagnostics and risk-reassessment increment is recorded below; full matrices and main CI remain pending. No commit, push or main adoption is claimed.

## Generic frozen diagnostics consumer qualification on e4e5039

The new focused contribution uses exact clean companion e4e50396876c5e7e85b52cdc3dbb510bab9c4f4d. All production/test LSP-specific method/type/field dependencies are removed; source searches include lsp_document, lsp_discovered, lsp_config_file, LspManager/Config/Server and PluginLsp. ReloadSummary maps every original frozen diagnostic, preserving severity and original Display/plugin/path, and only adds EKO retirement wording for component lsp. The original applicability and non-LSP Error rejection remain unchanged. No filesystem read, string parser, decoder or second state owner is introduced into the projection.

The complete-diagnostics regression actually failed at summary1 versus frozen2 (Cargo101), then the exact same command passed0 after the consumer change. Current modules pass 48 plugin +47 extension/real ts-rs export +6 configuration +6 watcher +4 developer +22 extension-owner tests: 133 distinct tests, zero failures/ignores/warnings. Two extra single-case successful executions check the original red entry and print the actual Unix I/O cases. Directory, broken link, metadata loop, read-permission and metadata-permission cases all actually ran under local uid501; no privileged fallback was taken. Valid/invalid YAML, InvalidUTF8, true absence, LSP-only, mixed components, cache reuse and invalidate boundaries are covered by the current tests. Current focused app-core lib Clippy (including strict no-panic lints), cargo fmt plus check and included-source rustfmt checks pass. This is not a full workspace/GUI matrix.

New isolated real exports match all five retained generated types exactly after formatting; no import/type source is changed. The approved three files remain deleted. Frontend source still matches its accepted candidate hashes, so the earlier 58-file/295-test/build evidence is retained only for those unchanged frontend inputs, not as new Rust/native evidence.

Independent source increment review and independent exact-six exposure reassessment returned pass0. The unchanged risk guard first rejected changed consumers before scanning. Only consumer_inputs_sha256 moves to ace3f77e95819525f7bc78d2ce1370a2c5b1d01120833122b9f99168278f1bd0; lock cca90ea328dbdbb5ebf819b5afb8845714c202e5f5fd1b9c032811c35f0e1307, all six identities/kinds, scanner and ignores remain unchanged. Six policy tests pass. Fresh actual-lock and canonical-origin scans on RustSec21d909e (1352 advisories/839 dependencies) each retain scanner1, zero vulnerabilities and exactly six accepted warnings, with separate gate0/noFetch=false. The default HTTPS fetch failure and successful process-only HTTP/1.1/exact-SSH-URL transport recovery are separately preserved; no global config change or no-fetch bypass.

This compilation batch was saved outside target and promptly cleaned after lsof found no active readers: cargo clean0 removed7.2GiB; target is absent. Raw e4e-* logs, original audit JSON/stderr/gates, e4e-run/risk-run receipts, source input hashes and new TS exports remain under the same ignored local task directory. No Host/native product artifact or real language server execution is claimed. Full workspace/all-feature/GUI/examples, SDK Outcome, exact main CI and publication remain pending under the coordinator; no push or main merge is authorized here.

Final independent review of the complete 57-path local focused contribution returned pass with zero action items. Its acceptance is limited to this contribution, not Plan04/main/full matrices. The official current staged task binding and normal explicit receipt are finalized after this review; no source or engineering claim is inferred from the receipt alone.
