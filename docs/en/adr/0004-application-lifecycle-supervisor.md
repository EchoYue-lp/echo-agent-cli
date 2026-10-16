# ADR 0004: Application Lifecycle Supervisor

- Status: Accepted
- Date: 2026-08-24

## Context

GUI, TUI, CLI/JSONL, and channels share process resources, but historical
entrypoints maintained separate shutdown lists. That caused owner leaks and a
deadlock when delivery waited for foreground settlement while the application
waited for delivery first.

## Decision

Use an app-core `ApplicationLifecycleOwner`. It binds resources immediately
after successful bootstrap and performs rollback for partial startup. Shutdown
has two phases: synchronously close admissions and broadcast cancellation, then
join accepted owners and aggregate every error into a typed receipt.

Product-data I/O uses the shared owned flow and bounded blocking adapter.
Semaphore capacity is not an operation owner. Caller cancellation only drops a
waiter; accepted work continues to durable settlement.

### Unsettled execution observations

`ExecutionNotSettled` is an observation of the original execution, not a failure
or cancellation receipt. The shared foreground, TaskRun and analysis owners
retain their original generation, pool, memory and I/O receipts; surfaces use
existing error presentation without publishing a terminal or replaying work.
A live cancellation registration also remains nonterminal before its driver
starts. Shutdown reports unresolved ownership and does not release it.

Before work, the existing Task journal or private analysis-directory intent
records the original owner identity. Restart admission honors that fact. Only
matching physical completion permits durable terminal recording and intent
cleanup. Known completion with failed persistence uses the same owner's
serialized cleanup retry; it never runs the Agent or tool again. Foreign
unknown execution without a completion source remains explicitly fenced.


The original application-generation root strongly retains the same lifecycle
Owner and execution receipt bundles; child registries and progress links are
Weak. Its real cleanup producer drains independent ordinary flows and pool
leases, then reports unresolved exact ownership without closing dependencies
still needed by it. A metadata-only publication cannot be a fake final receipt.
The existing typed lifecycle error retains this root even after Runtime/Services
are dropped. Genuine same-owner evidence resumes only original cleanup; only
actual completion releases resources. Opaque workspace I/O guards retain their
original counter, while full scoped controls keep their Host/Task/Tool services.

## Consequences

Every process-level resource must bind an owner at creation and provide a
non-waiting close plus a waitable settlement. Framework cancellation primitives
are reused, while EKO admission order, surface hooks, and product receipts stay
in app-core.

Canonical ApplicationServices composition binds the same lifecycle root to the borrowed AppState's workspace registry, foreground turns and analysis supervisor before TaskRun boot recovery or an application scheduler producer can admit work. The final AppState Arc transfers strong ownership through the same binding helper; cold workspace stores never depend on compose having returned before admitting their original driver.
