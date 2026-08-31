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

## Consequences

Every process-level resource must bind an owner at creation and provide a
non-waiting close plus a waitable settlement. Framework cancellation primitives
are reused, while EKO admission order, surface hooks, and product receipts stay
in app-core.
