# ADR 0037: Unified Turn-Run Binding

- Status: Accepted
- Date: 2026-09-03
- Owners: `chat_driver`, `tasks/task_runtime`

## Context

After removing Chat/Task/Auto interaction modes, ordinary turns could still run without a
TaskRun. They received no durable Goal projection, a lazily created run did not preserve its
identity across continuation turns, and runtime deferral/wake behavior covered only explicitly
bound work.

The convergence research compared Codex Thread/Turn/Item semantics with Claude Code queued
messages, task lists, and background execution. Claude Code delivers queued messages in the same
turn after tools finish or as a subsequent turn, while task and background views remain separate.
These systems support one turn driver and artifact-based planning, but they do not justify
inferring run purpose from the absence of a plan.

- Claude Code: <https://code.claude.com/docs/en/interactive-mode#queue-messages-while-claude-works>
- Repository snapshots: [Codex capability catalog](./0002-codex-tool-capability-catalog.md) and
  [Claude Code capability catalog](./0003-claude-code-capability-catalog.md).

## Options

1. Add a second lightweight Goal/summary mechanism for run-less turns.
2. Eagerly bind every store-backed turn to `taskrun:{turn_id}` and retain typed run provenance.

## Decision

Choose option 2:

- `RunCreated` records `TaskRunExecutionProfile { provenance, plan_policy }`.
- Only `ConversationTurn` without a plan receives trivial `Completed + Stop` settlement.
  `Orchestrated + AllowDirect` may legitimately be planless while running or recovering.
- Boot recovery atomically cancels an interrupted conversation turn, completes a persisted
  turn-settlement debt, and pauses an interrupted orchestrated run.
- The foreground owner carries the exact run id so GUI, TUI, CLI, channel, and user-authored live
  Agent deliveries read the original user text before `PreparedUserTurn` rewriting, then apply the
  journal's UTF-8-safe 2,000-character retention bound. Agent-authored messages are not user
  constraints.
- Quiet runtime observation and continuation resume are committed under one run lock.
- Oversized Goal artifacts use atomic writes, revision/hash-bound names, and digest validation.
- Planless conversation runs remain in the journal but are excluded from the task UI projection;
  `PlanRevisionCommitted` activates the task UI when such a run publishes a plan.

## Trade-offs

Each turn creates a run directory and additional journal events. This cost buys one admission,
Goal, recovery, and audit path. If file volume becomes material, storage tiers should be optimized
without restoring a run-less execution path.

## Scope

This is EKO application policy. The reusable `echo-agent` turn and task primitives do not change.
The implementation affects chat driving, foreground identity, TaskRuntime recovery and wake
logic, generated TypeScript contracts, all interactive surfaces, and persistence documentation.
