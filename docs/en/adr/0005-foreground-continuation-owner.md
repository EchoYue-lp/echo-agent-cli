# ADR 0005: Foreground Owner for RunTurn Continuations

> Status: Accepted
>
> Date: 2026-08-24

## Context

A long-running TaskRun can span multiple finite RunTurns. Releasing the
foreground lease after the first turn leaves later turns without a root owner,
stable cancellation, or a surface-visible settlement.

## Decision

`ForegroundTurnLease` is the sole settlement capability for the complete
foreground operation. Continuation requests share one dispatch and return
Started or Joined receipts. Later turns carry a non-owning progress handle,
reuse the root cancellation token, and update only the active turn ID. Deferred
ends the foreground operation but not necessarily the TaskRun.

## Consequences

Spawn is an execution boundary, not proof of ownership transfer. Detached or
recovery work cannot register a second launcher, foreground registry, or
completion reducer. Cancel, steer, retry, and shutdown all observe the same
root owner and durable terminal facts.
