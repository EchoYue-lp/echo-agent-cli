---
schema_version: 1
id: finding.cli-upstream-dependency-risks
kind: finding
type: implementation_bug
status: risk_accepted
severity: high
primary_focus: contract_evidence
focus: [failure_concurrency, time_lifecycle, contract_evidence]
boundary_ref: boundary.eko-framework-consumer-settlement
behavior_refs: []
rule_refs: [rule.eko-framework-consumer-authorities]
evidence_refs: [evidence.cancellation-observation-consumer]
audit_refs: []
decision_refs: [docs/supreme/specs/cancellation-observation-consumer/dependency-risk-decision.md]
repair_evidence_refs: []
verification_evidence_refs: []
rereview_audit_refs: []
discovered_at: c874a79aa25f8bef16ba4df2e809ceb447bb1604
---

# Current CLI upstream dependency risks

## 问题

The actual lock and conservative registry-origin view contain glib0.18.5/RUSTSEC-2024-0429 (unsound), proc-macro-error1.0.4/RUSTSEC-2024-0370 (unmaintained), and bincode1.3.3/RUSTSEC-2025-0141 (unmaintained). Exact checksums and GUI/macro/TUI consumers are in the decision reference.

## 触发条件与影响

glib的字符串Variant迭代存在实际soundness风险；GUI宏依赖和TUI dump序列化依赖另有维护中止风险。依赖可达性及当前目标/feature见人的精确Decision，不以本地定位推不存在风险。

## 证据

Official unfiltered scans actually returned exit1 with three denied warnings. The user explicitly accepted keeping these stable dependencies and deferring upgrades. This is risk_accepted, not resolved or false_positive; no fix or clean audit is claimed.

## 处理记录

Only the named current versions, registry sources/checksums and consumer graph are accepted. Changed inputs require reassessment; extra warnings, vulnerabilities or yanked results are outside this decision. Exact CI gate wiring remains pending verification, and all source/test/lint failures remain blockers.


## 2026-10-21 additional exact human acceptance

用户另行明确接受当前锁d2e7 / consumer e46的zbus_names4.3.4/RUSTSEC-2026-0371、zvariant5.13.1/2026-0370、zvariant_derive5.14.0/2026-0372精确维护风险及固定六项合同。source/checksum/kind与scope以原Decision和policy为准；原三项版本、全部接受字段和事件期限不变。此后此Finding当前risk_accepted集合为精六项，不把新维护风险写resolved，也不把旧preapprovalgate1改为成功。

批准后actual/canonical-origin两扫描分别0vuln/6精warnings，原scanner1与独立riskgate0/caller0均真实保留；刚独立核验的officialDB e6f94/tree36f251通过ownDB/no-fetch固定，不冒onlinefetch。新的0382漏洞已通过WebPKI0.103.16精两值lockpatch消除于该DB扫描，不能据审计冒patched-lock工程/最终main通过。额外或变化的告警、漏洞、yanked与输入/来源/类别漂移仍阻断；未满足的源码/完整门禁/nativeHook/最终CI均不豁免。
