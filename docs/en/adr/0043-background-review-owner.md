# ADR 0043: EKO Owns Background Review Admission and Settlement

## Status

Accepted

## Context

The framework now returns a lazy `BackgroundReviewHandle` with a stable `ReviewIdentity`. Its optional memory write may commit before the caller observes a `ReviewOutcome`. Dropping a caller or exiting the process after that write otherwise leaves no application-level terminal receipt. The framework's [ADR 0058](https://github.com/EchoYue-lp/echo-agent/blob/main/docs/adr/0058-background-review-settlement-ownership.md) assigns admission, shutdown, and outcome settlement to the embedding application; [ADR 0065](https://github.com/EchoYue-lp/echo-agent/blob/main/docs/adr/0065-evolution-memory-audit-reconciliation.md) provides Memory journal reconciliation and exact-key lookup, not an EKO review lifecycle.

Tokio's [graceful shutdown guidance](https://tokio.rs/tokio/topics/shutdown) separates cancellation from waiting for owned tasks to finish. Its [`JoinHandle::abort` contract](https://docs.rs/tokio/latest/tokio/task/struct.JoinHandle.html#method.abort) likewise requires awaiting the handle to observe cancellation. EKO already owns a generation lease and Review Inbox, so this lifecycle belongs in app-core rather than the reusable framework or a surface adapter.

## Options

1. Let GUI, TUI, and CLI spawn their own review tasks. Rejected: observer loss and process exit have no shared settlement or recovery authority.
2. Move EKO receipt and Review Inbox policy into `echo-agent`. Rejected: workspace generation, shutdown, and UI evidence are application-specific.
3. Admit one lazy handle into an app-core owner with a durable receipt journal (chosen).

## Decision

1. GUI, TUI, and CLI keep their existing ordinary review interactions, but all pass the framework lazy handle to `ReviewGenerationLease::track_background_review`. Side Conversation remains GUI-only and unrelated to this core review capability.
2. The owner captures the stable Memory key's pre-admission fingerprint, then writes `Admitted(operation_id, ReviewIdentity, authority_scope, workspace_generation, memory_before)` with confirmed durability before polling the handle. Duplicate active reviews of the same run are rejected; closed admission never polls a handle. Recovery rejects a pending receipt from another workspace generation.
3. A supervisor retains the generation lease. It records the complete `ReviewOutcome` before projecting its supported candidate into the existing Review Inbox, then writes a terminal receipt. The Inbox upsert is idempotent, so recovery can replay an outcome after a crash between the two writes.
4. Caller drop or application shutdown aborts the inner task, awaits it, reconciles the framework Memory journal, and compares the stable persistence key with the pre-admission fingerprint. The receipt distinguishes a newly observed write, no write, and an indeterminate pre-existing value; it never attributes a previous review's memory to the interrupted operation. No missing quote or candidate is fabricated.
5. Startup replays unfinished admissions before the primary Agent is published. A durable outcome is projected and marked settled; an admission without outcome is marked interrupted after Memory reconciliation. Recovery failure blocks bootstrap rather than silently discarding the debt.
   The owner exposes a typed read-only receipt by operation ID, and GUI review responses include that ID for later inspection.
6. The receipt journal is EKO-owned file state under `.eko/evolution/background-review.jsonl`. It is not a second Memory or Review Inbox authority. Review analysis, confidence policy, and memory transaction semantics remain in the framework.
7. Desktop shutdown uses Tauri's [`App::run_return`](https://docs.rs/tauri/2.11.2/tauri/struct.App.html#method.run_return), then awaits the existing `ApplicationServices` settlement before the desktop entry returns. `Builder::run` calls the process-exiting `App::run` and cannot be followed by asynchronous cleanup on normal exit. Startup failure and a nonzero loop exit also settle the same owner and preserve the failure in its receipt.
8. When bootstrap memory resolves to an existing workspace's `.eko`, recovery uses that workspace's persisted scope and generation. The primary pool and the workspace execution host share the matching ReviewIntegration and Store; bootstrap must not recover scoped debt as `global/global`, or create a second admission owner for the same binding. A retired owner stays closed, and unrelated-generation debt still fails recovery.
9. Memory attribution compares a versioned fact fingerprint that excludes recall counters and timestamps. An existing legacy full-metadata fingerprint cannot confirm this review's write; recovery reports unknown instead. Bootstrap rollback settles the command-cell service's owned ProductData flow before awaiting aggregate ProductData shutdown.
10. Reusing the owner must retain rule publication for all live primary Agents and pools. Targets are weak and deduplicated so retiring a workspace cannot retain its Agents, and a later workspace binding cannot replace bootstrap targets. Publication prepares every pool before changing Agent instructions. An unchanged project link does not retire the Host or close its shared Review admission.
11. Each projection target keeps its own resolved project root. An explicit bootstrap `--project Q` remains authoritative for that primary and seed pool, while workspace Agents use their persisted project P. Both combine their project instructions with the same workspace-owned learned rules and Memory state. Target snapshots are read strictly before publication; they may have different revisions because their project inputs differ.

## Consequences

- A committed memory effect can remain without reconstructible review evidence after process death. The terminal receipt makes that uncertainty observable and permits an explicit fresh review; it does not claim that the original review succeeded.
- Skill lifecycle changes required by the same framework update use the framework `SkillMutationAuthority` and a dedicated EKO business ChangeLog. This does not make Background Review a Skill mutation.
- Framework examples and `echo-website` do not consume EKO's application-only receipt API; no update is needed there.

## Verification

Review tests drive a real lazy `BackgroundReviewHandle` through app-core admission and park the Memory observer after the durable write. Caller drop and shutdown must both join the child, expose an interrupted receipt with the committed Memory effect, and avoid inventing an Inbox candidate. Bootstrap regressions reopen scoped debt using the persisted workspace identity and check owner reuse. Desktop exit tests exercise the real application lifecycle owner through the same returning-loop adapter; they do not claim to automate an OS window or forced process termination.
