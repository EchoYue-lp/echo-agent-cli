# ADR 0007: TaskRuntime Agent Turn Authority

Status: Accepted

## Context

Two app-level raw Agent stream loops independently inferred terminal state,
usage, failure, and retry. Missing terminal events could look successful, and
Goal completion could race an active RunTurn.

## Decision

Framework-owned `AgentTurnDriver` exclusively starts raw streams, sequences
events, records typed terminal and cancellation, and preserves provider
receipts. EKO `EkoAgentTurnSink` is the sole adapter for events, usage, tools,
evidence, and artifacts. `turn_lifecycle.rs` is the sole EKO RunTurn terminal
service. PlanTask and physical Subagent terminals remain distinct.

Independent planning disables every Write/Execute capability before invocation,
including dynamic Plugin/MCP tools; dynamic Read tools remain available.

## Consequences

Typed provider failures become durable evidence and retry uses a stable
fingerprint. Direct completion cannot claim a mutation. Framework trace has no
Paused variant, so EKO never maps a paused trace to Completed.
