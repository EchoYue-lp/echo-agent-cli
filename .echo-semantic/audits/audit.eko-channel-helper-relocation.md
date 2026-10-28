---
schema_version: 1
id: audit.eko-channel-helper-relocation
kind: audit
boundary_ref: boundary.eko-framework-consumer-settlement
lens: contract_evidence
freshness: examined
revision: source:4726a37cc1350e66b30b281fd02e16cdd087cd7f44b58b28a0ba6ab09bfa0d46
finding_refs: [finding.eko-channel-helper-relocation]
challenges:
  helper-relocation-loses-implementation:
    source_refs: [echo-agent-app-core/src/channel_runtime/input_pump.rs, echo-agent-app-core/src/channel_runtime/outbound.rs, echo-agent-app-core/src/channel_runtime/tool_render.rs]
    evidence_refs: [evidence.eko-channel-helper-relocation]
  old-caller-still-owns-a-parallel-channel-runtime:
    source_refs: [docs/zh/adr/0050-gui-product-convergence.md, echo-agent-app-core/src/channel_runtime.rs, Cargo.toml, echo-agent-app-core/Cargo.toml]
    evidence_refs: [evidence.eko-channel-helper-relocation]
---

# Three helper source relocation review

## 审查范围

Independent depth_source_review source-only deletion increment: the three old bin helper locations and their moved original implementation, module declarations, feature forwarding and retained CLI forward.

## 已检查故障假设

A helper was lost or semantically changed while moving; module/feature callers still select the old implementation; a second state owner or queue was added to work around the move.

## 实际实现路径与证据

Each original HEAD28bb helper was compared with its app-core counterpart. Differences are crate ownership paths and formatting. The old files are absent, the existing implementation is reachable through app-core/channel_runtime, and the CLI forwards to it. This narrow source-only increment passed with zero action items.

## 问题记录

The complete backend v1 review was separately blocked by three Important runtime-path regressions. They do not change the mechanical helper-file comparison and must be repaired and reviewed separately before full source acceptance.

## 残余风险

Compilation and real execution of this combined source are not established. Old qualified source receipts remain historical and do not qualify this integration.

## 未检查项

Cargo/Clippy/Rust regression execution, actual Rust-generated exports, native GUI/live IM, fresh exact dependency risks, full matrices, main and CI. None was performed by this source-only audit.


## 2026-10-29：退役后的前置路径导航维护

本节只维护结构化源码/消费者引用的导航，不重新观察或续期旧运行资格。原观察 `source:4726a37cc1350e66b30b281fd02e16cdd087cd7f44b58b28a0ba6ab09bfa0d46`、上文历史原件、失败与限制、原 command/scenario/after revision 均保持原范围；本节不新增 matched、状态或 owner。

| 原历史路径 | 当前实现或退役决定导航 |
| --- | --- |
| `src/cli/channels.rs` | `docs/zh/adr/0050-gui-product-convergence.md` |

旧路径身份与退出范围由既有退役资产/Finding和正式ADR0050/0057保留；源码历史可在 `pre-retirement-2026-10-24` 及后继具名Git保存点回读。新导航只指向现存原接管实现或退出决定，不能把旧observed_at解释成这些路径当时已经运行通过。unknown/needs_review、原动态/native/风险缺口不清；本候选没有执行Cargo或官方新Task校验，采用与真实来源绑定由EKO唯一writer按正常新Task合同完成。
