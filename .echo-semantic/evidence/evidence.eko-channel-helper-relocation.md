---
schema_version: 1
id: evidence.eko-channel-helper-relocation
kind: evidence
observed_at: source:4726a37cc1350e66b30b281fd02e16cdd087cd7f44b58b28a0ba6ab09bfa0d46
source_refs:
- echo-agent-app-core/src/channel_runtime.rs
- echo-agent-app-core/src/channel_runtime/input_pump.rs
- echo-agent-app-core/src/channel_runtime/outbound.rs
- echo-agent-app-core/src/channel_runtime/tool_render.rs
- echo-agent-app-core/Cargo.toml
- src/cli/channels.rs
- Cargo.toml
- docs/en/adr/0053-im-cron-gui-control.md
- docs/zh/adr/0053-im-cron-gui-control.md
supports: [finding.eko-channel-helper-relocation, rule.eko-framework-consumer-authorities]
limitations: [Source-only mechanical relocation and caller/feature switch, No Cargo compile or test execution, No native or live IM acceptance, No risk acceptance renewal or whole Plan07 closure]
evidence_type: behavior_equivalence
before_revision: 28bb9459bde9872b8d36cdd4a6eb98929feb0927
after_revision: task:bf4233732cc71c014f907ae38f15ea0eeb393cc27ac4e0d62ab4c9fb7ecf88e6
deleted_paths: [src/cli/channels/input_pump.rs, src/cli/channels/outbound.rs, src/cli/channels/tool_render.rs]
coverage: [Only three helper source bytes after crate-path rewrite and formatting, Original module and feature caller switch reviewed as source, No claim of compiled or executed runtime equivalence]
scenario_results:
  input-pump-source-bytes:
    status: matched
    source_refs: [echo-agent-app-core/src/channel_runtime/input_pump.rs]
  outbound-source-bytes:
    status: matched
    source_refs: [echo-agent-app-core/src/channel_runtime/outbound.rs]
  tool-render-source-bytes:
    status: matched
    source_refs: [echo-agent-app-core/src/channel_runtime/tool_render.rs]
command_results:
  - { command: 'python3 .echo-semantic/local/gui-runtime-app-integration/verify-helper-relocation.py', exit_code: 0 }
---

# Original channel helper source relocation

## 支持的结论

Only the three original bin helper files are relocated to app-core/channel_runtime: input_pump.rs, outbound.rs and tool_render.rs. Independent reviewer depth_source_review compared each actual file with HEAD28bb: differences are crate ownership paths and formatting. The original channel_runtime module declarations, callers, root channels feature forwarding and thin CLI entry select the moved implementation. No parallel owner was introduced.

## 来源与范围

The source-only deletion increment independently passed with zero action items for these three paths. Direct rustfmt parsed the source; current path search leaves only the explicitly historical completed plan reference at the old path. Immutable old v3/Depth/SSH sources remain unchanged. Actual frontend focused tests/types/lint and docs checks are separate evidence, not Rust proof for this relocation.

## 已知缺口

This narrow evidence closes obsolete-source-path relocation only. The first complete backend source review separately found three Important runtime-path regressions; their repairs and new source review are tracked separately. No Cargo command compiled this combined tree, no Rust regression definition is marked executed, and no main/native/platform qualification follows from this document.

Actual normalized source checks (not runtime):

- `echo-agent-app-core/src/channel_runtime/input_pump.rs` SHA `d8a58311575fda50780857e4f352b10e52c6d3cabe22b112d52d08c81163d593` exactly equals the original helper after the documented crate-path rewrite and Rust formatting.
- `echo-agent-app-core/src/channel_runtime/outbound.rs` SHA `3ca258d1d550c45d960c61f05adcb71606ad995925b3979373ca206aca8519f8` exactly equals the original helper after the documented crate-path rewrite and Rust formatting.
- `echo-agent-app-core/src/channel_runtime/tool_render.rs` SHA `1140303c8d3a2e5274464cbfb54dc282bf6136bcbfe97d22cf71c2270f2274c9` exactly equals the original helper after the documented crate-path rewrite and Rust formatting.
