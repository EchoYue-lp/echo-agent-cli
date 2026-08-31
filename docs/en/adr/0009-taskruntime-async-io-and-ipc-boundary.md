# ADR 0009: TaskRuntime Async I/O and Typed IPC Boundary

Status: Accepted

## Context

TaskRuntime file journal operations perform open, recovery, fsync, checkpoint
flush, and projection reads. They must not run directly on Tokio executor
threads. GUI mutations also returned untyped JSON and omitted stable continuation
fields from frontend declarations.

## Decision

All synchronous TaskRuntime I/O crosses the bounded
`TaskRuntimeOperation`. IPC commands return generated typed receipts and
use typed enums for control fields. Each blocking closure captures the exact
workspace authority and accepted operations outlive caller futures.

## Consequences

Async surfaces cannot infer terminal state from a future return or UI lifecycle.
The same receipt and serialization contract is shared by GUI, TUI, CLI/JSONL,
channels, recovery, and workspace deletion.
