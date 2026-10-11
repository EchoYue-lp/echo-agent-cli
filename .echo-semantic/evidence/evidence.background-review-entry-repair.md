---
schema_version: 1
id: evidence.background-review-entry-repair
kind: evidence
observed_at: source:2b2aadbb89e3fb339505cc4e42f53c0466f982f1310e5fc1c30720acaefa33c3
source_refs: [src/tauri/desktop.rs, echo-agent-app-core/src/runtime.rs, echo-agent-app-core/src/workspace/runtime.rs, echo-agent-app-core/src/agent_pool/pool.rs, echo-agent-app-core/src/evolution/review_integration.rs, echo-agent-app-core/src/evolution/background_review_owner.rs, echo-agent-app-core/src/state/app_state.rs, echo-agent-app-core/src/state/tests.rs, docs/en/adr/0043-background-review-owner.md, docs/zh/adr/0043-background-review-owner.md]
supports: [finding.background-review-entry-settlement, behavior.background-review-caller-owned-settlement, rule.eko-framework-consumer-authorities]
limitations:
  - Remote CI and mainline delivery remain pending
  - Forced process termination cannot return an in-process ReviewOutcome
---

# Background Review entry repair

## 支持的结论

The candidate uses Tauri `build -> run_return -> ApplicationServices.settle` for
normal desktop exit and returned build errors. It reuses the existing service
owner; no second framework supervisor is added. Bootstrap reads only the
manifest at the actual Memory state root, validates the canonical root, and uses
the persisted scope/generation. The workspace Host borrows the matching seed
ReviewIntegration and Store, skips recovery of live receipts, and refuses a
retired owner before publishing a new execution runtime. Publication checks that
detect closed admission or degraded hot-memory projection explicitly settle the
newly constructed runtime. Bootstrap command-cell shutdown releases its
ProductData flow before aggregate ProductData shutdown is awaited.

The same owner retains weak, deduplicated primary/pool rule targets. Publication
prepares all pool admissions before modifying targets and supports retry of the
existing durable rule effects. Unchanged project links retain the Host and its
Review admission while continuing existing refreshes. A versioned Memory fact
fingerprint excludes recall telemetry; legacy fingerprints remain attribution
unknown. The original synthetic observer test is replaced by a real lazy handle
parked inside `on_memory_write`, through actual app-core admission and supervisor.

## 来源与范围

All lifecycle, UI receipt, and project projection adaptation is EKO application
code. Framework ReviewIdentity, lazy execution, tri-state persistence, and Memory
journal authority remain unchanged. Explicit project contexts belong to each
projection target, not the shared Memory/review identity. Bootstrap targets retain
explicit project Q while workspace targets retain persisted project P. Strict
target-context preflight rejects existing unreadable inputs before new durable
rule effects; post-effect reads still retain EffectsApplied recovery on failure.

## 已知缺口

Examples and echo-website do not consume this private app owner/desktop adapter,
so they require no API update. Tests do not operate a real OS window, restart
Tauri, or prove cross-target atomic rollback when a publication future is
cancelled. Durable EffectsApplied remains the recovery source for interrupted
rule publication. All applicable local gates passed; this remains a candidate
record until remote CI and delivery finish.
Earlier post-fork primary/rule initialization errors still return directly; their
complete cleanup is an existing, unproven coverage limit, not a verified property
of this repair.
