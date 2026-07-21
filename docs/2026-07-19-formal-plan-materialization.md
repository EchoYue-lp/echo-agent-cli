# Formal Plan Materialization Count Contract

Status: Superseded on 2026-07-21 by
`docs/2026-07-21-dynamic-plan-runtime.md`.

The count handshake described below fixed the hidden-dispatch bug, but the
current contract replaces repeated per-node creation with one atomic DAG and
uses `plan_revision` for optimistic concurrency and exact execution identity.

## Problem

The main Agent could say it was dispatching N parallel Subagents while the
TaskRuntime contained fewer PlanTask nodes. The old `plan_execute({task})`
shortcut created an independent one-task Run for every call, while the formal
run and right task panel observed only the PlanTasks persisted through
`plan_create`. This produced three conflicting views: prose said N, the main
timeline hid the dispatch calls, and the task panel showed only the persisted
subset.

## Reference Pattern

- [Claude Code subagents](https://code.claude.com/docs/en/sub-agents) treats
  delegated work as explicit subagent invocations with isolated context and a
  returned result.
- [OpenAI Codex app-server](https://github.com/openai/codex/blob/main/codex-rs/app-server/README.md)
  exposes stable thread, turn, and item events rather than deriving execution
  state from assistant prose.
- The existing EKO orchestration decision in
  `docs/2026-07-17-domain-subagent-orchestration.md` already established that a
  plan is an observable artifact and terminal execution facts come from the
  runtime event stream.

The shared pattern is that UI state follows materialized work items and runtime
events. A prose claim or an alternate hidden dispatch route is not an execution
fact.

## Application Boundary

This repair belongs to `echo-agent-cli` TaskRuntime. The generic `echo-agent`
framework already provides reusable Subagent execution and does not need EKO's
plan-count or task-panel contract.

## Decision

1. `plan_execute` executes only the current persisted PlanTask DAG. The inline
   `task` parameter and implicit one-task fallback are deleted.
2. One `plan_create` call materializes the complete initial PlanTask DAG and
   reports revision 1.
3. The TaskRun already owns the user goal. Planning must not add a wrapper or
   placeholder PlanTask for that goal; every node represents actual Subagent
   work.
4. Planning calls `task_list` and passes the exact committed revision as
   `plan_revision`; later changes use `plan_patch(base_revision=...)`.
5. `plan_execute` rejects empty plans and revision mismatches before dispatch.
6. One ad-hoc isolated subtask remains the responsibility of `agent_tool` in
   Chat mode. Auto and Task mode hide `agent_tool`; any delegation in those
   modes must use the formal plan so TaskRuntime and the right panel remain the
   canonical execution view.
7. The Agent must not claim dispatch before `plan_execute` accepts the complete
   plan.

This keeps one canonical product path: `TaskRun -> PlanTask -> SubagentRun`.
The main timeline and right panel project the same persisted plan.

## Verification

- Default workspace check and tests passed: 528 app-core tests, CLI/TUI tests,
  integration tests, and doctests.
- All-feature workspace check and tests passed, including GUI and channel
  targets.
- No-default, TUI, channels, telemetry, GUI, and GUI+devtools feature checks
  passed.
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`
  passed.
- Frontend 62 tests, TypeScript build, Web build, and Tauri build passed.
