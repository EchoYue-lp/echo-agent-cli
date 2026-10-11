---
schema_version: 1
id: finding.background-review-entry-settlement
kind: finding
type: implementation_bug
status: resolved
severity: high
primary_focus: time_lifecycle
focus: [state_authority, data_durability, failure_concurrency, contract_evidence]
boundary_ref: boundary.eko-framework-consumer-settlement
behavior_refs: [behavior.background-review-caller-owned-settlement]
rule_refs: [rule.eko-framework-consumer-authorities]
evidence_refs: [evidence.background-review-entry-repair, evidence.background-review-entry-verification]
audit_refs: [audit.background-review-entry-independent-rereview]
decision_refs: []
repair_evidence_refs: [evidence.background-review-entry-repair]
verification_evidence_refs: [evidence.background-review-entry-verification]
rereview_audit_refs: [audit.background-review-entry-independent-rereview]
discovered_at: ff27188e828c1685337dfd6fcc80b207a6456d6a
---

# Background Review entry and recovery settlement

## 外部 Issue

https://github.com/EchoYue-lp/echo-agent/issues/38

## 问题

The app-core durable owner exists, but desktop normal exit uses the process-exiting
Tauri `Builder::run`, so subsequent service settlement is unreachable. Bootstrap
also opens workspace-local receipts as `global/global`, rejecting the actual
persisted workspace generation before the correct Host can recover them. Its
rollback waits for ProductData before the command service releases its owned flow.

## 触发条件与影响

Normal GUI exit can truncate accepted reviews. Restart from a workspace cwd with
an unfinished scoped receipt rejects valid recovery and can hang during rollback.
Sharing the corrected owner must preserve every rule projection target, explicit
project overrides, same-project link idempotence, and closed-admission fences.

## 证据

ADR 0043 and the repair/verification evidence trace the actual desktop,
AgentRuntime, workspace Host, ReviewIntegration, and receipt-journal paths.
Historical PR #5/#7 and their CI prove the earlier delivery, not these newly
discovered entry conditions. The external Issue was reopened for this repair.

## 处理记录

The candidate repair is resolved after two independent implementation reviews
and all applicable complete local gates. External Issue #38 stays open until
remote CI and mainline delivery are verified. Process death before durable
Outcome remains Interrupted/unknown; no fabricated success or Inbox candidate.
