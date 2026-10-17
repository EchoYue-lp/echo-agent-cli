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
