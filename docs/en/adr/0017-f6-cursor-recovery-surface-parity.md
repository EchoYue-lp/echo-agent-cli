# ADR 0017: F6 Cursor, Recovery, and Surface Parity Closure

## Context

F1-F5 had converged the input receipt, Task/Plan/Todo, Agent control, and
Agent/Subagent lifecycle contracts, but no single executable fixture proved
cursor restart, cold addresses, workspace incarnation, boot reconcile,
exactly-once terminal, and five-surface parity together.

## Decision

Conversation cursors remain bound to complete target identity and router
sequence. TaskSubagent cursors remain bound to workspace, run, task, revision,
execution, attempt, and generation. Cold targets are proven by the workspace
ConversationStore; delete/recreate uses a new opaque generation. Boot recovery
is success-only singleflight. GUI, TUI, CLI/JSONL, and channels replay one
durable event fixture and accept one typed terminal.

## Consequences

No new store, cursor authority, state machine, or public framework API is
introduced. Renderer differences are presentation-only; terminal meaning comes
from durable facts and not text, EOF, or surface-local flags.
