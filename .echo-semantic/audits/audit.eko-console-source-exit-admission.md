---
schema_version: 1
id: audit.eko-console-source-exit-admission
kind: audit
boundary_ref: boundary.eko-framework-consumer-settlement
lens: contract_evidence
freshness: examined
revision: source:513d810144fb2c3348549b4734c9ae9275edd7cfbe1c152419b76db135f871f3
finding_refs:
- finding.eko-console-source-exit-admission
challenges:
  exclusive-leaf-classification:
    revision: source:513d810144fb2c3348549b4734c9ae9275edd7cfbe1c152419b76db135f871f3
    source_refs:
    - Cargo.toml
    - src/lib.rs
    evidence_refs:
    - evidence.eko-console-source-exit-admission
  retained-owner-backedge:
    revision: source:513d810144fb2c3348549b4734c9ae9275edd7cfbe1c152419b76db135f871f3
    source_refs:
    - src/tauri/desktop.rs
    - echo-agent-app-core/src/auto_memory/mod.rs
    - src/tauri/commands/panels.rs
    - echo-agent-app-core/src/runtime.rs
    - echo-agent-app-core/src/scheduler/runner.rs
    - src/tauri/mod.rs
    - echo-agent-app-core/src/terminal.rs
    evidence_refs:
    - evidence.eko-console-source-exit-admission
---

# 独立终端源码删除准入审查

## 审查范围

仅审查具名54独占源删除、8共享修改、原共享owner接线和恢复材料；不覆盖删后的动态等价。

## 实际实现路径与证据

Gov及独立reviewer对精确62候选SOURCE PASS C0/I0/M0；本SID重新核62基底、54恢复原件、真实dry-after-view和7保留锚文件。这里只反驳源码范围/保留backedge丢失假设，不声称新组合已经编译或原生运行。

## 已检查故障假设

已检查按目录误删共享能力、原owner被替换、after-view未实际生成、删除前字节无法恢复四类源码故障；检查依据与四个scenario_results一致。

## 问题记录

`finding.eko-console-source-exit-admission`只结算源码删除准入；旧LSP/helper的resolved未被复用为本次授权，其他动态与needs_review对象未被清空。

## 残余风险

恢复固定pre-retirement-2026-10-24 tag与本次62原件，不改其它分支/refs/用户数据；若后继动态失败，原Goal保持开放并沿唯一writer修复。

## 未检查项

当前未执行删后Rust、依赖风险、最新Framework组合、GUI原生、手机、SSH或main CI；已有旧App基线不覆盖新源码。

正式采用后独立范围仍仅上述源码切点；54删除与8候选SHA匹配，未将任何动态场景改matched。
