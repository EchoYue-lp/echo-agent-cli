# Dynamic Plan Runtime

Status: Complete

## Decision

EKO keeps structured plans. It does not introduce a Markdown plan file.
`events.jsonl` remains the recovery authority, while two projections separate
the plan specification from execution state:

- `plan.json`: the latest immutable `PlanRevision` specification.
- `run-state.json`: `TaskRun`, per-task execution, and attempt state.

This is an application-layer capability in `echo-agent-cli`. The reusable
`echo-agent` framework continues to provide Subagent execution, cancellation,
tool capability primitives, and result events without importing EKO's plan
revision model.

## Authority

- The user and main Agent may submit a complete plan or a revisioned patch.
- A Subagent may report results, evidence, blockers, and task suggestions. It
  never mutates the plan directly.
- The executor mutates execution state only.
- `TaskRuntimeStore` validates and atomically commits plan revisions.
- Projection code is deterministic and performs no authorization decisions.

## Runtime Contract

1. `plan_create` submits the complete DAG in one call.
2. `plan_patch(base_revision, operations, reason)` performs optimistic
   concurrency control and commits one new revision.
3. `plan_execute(plan_revision)` executes exactly that committed revision.
4. Running and completed task specifications are immutable. Pending and
   blocked nodes may be revised at scheduler safe points.
5. Completed attempts are never restarted by a plan revision. Only explicit
   retry creates the next attempt.
6. Effective task tools are the intersection of the Subagent role tools and
   the task allowlist. An empty task allowlist means the role defaults.

## Validation

Every candidate plan is validated as a whole before an event is appended:

- stable unique task ids;
- non-empty titles and descriptions;
- known Subagent roles and tools;
- valid acyclic dependencies;
- task/tool capability compatibility;
- file ownership and write-wave constraints;
- executable checks, semantic acceptance criteria, artifacts, and retries;
- mutation safety against running and completed executions.

## Safe Points

The scheduler reloads the latest committed revision before the first wave,
after each wave, after a review result, and before a paused run resumes. An
active wave is never implicitly restarted. Run completion is committed only
after a locked recheck confirms that the latest revision has no running or
runnable tasks. Subagent suggestions remain advisory evidence in the execution
summary; the main Agent or user may promote them through `plan_patch`, but they
do not silently expand or block the plan.

## Industry Basis

Claude Code separates plan review, shared task tracking, and independent
Subagent contexts. Cursor Plan Mode treats the plan as an editable artifact
before execution and gives Subagents explicit capability contracts. Current
Codex protocols keep goal state, turn-plan progress, and collaboration Agent
lifecycle as separate concepts. EKO follows those separation principles while
retaining its richer file-backed DAG and local desktop workflow.

References:

- Claude Code Subagents: <https://code.claude.com/docs/en/sub-agents>
- OpenAI Codex app-server protocol: <https://github.com/openai/codex/blob/main/codex-rs/app-server/README.md>

## Delivered

1. Add revisioned plan specification and independent run-state projection.
2. Replace repeated single-node materialization with atomic `plan_create` and
   add `plan_patch`.
3. Enforce revision, terminal-state, capability, and transition validation.
4. Reload revisions at scheduler safe points and prevent duplicate attempts.
5. Migrate GUI, TUI, CLI, and channel projections to the same service.
6. Delete obsolete count handshakes, per-node plan tools, and redundant
   snapshot rewrites.

GUI, TUI, CLI, and channel entry points share the same application service and
file-backed authority. The frontend displays the active plan revision and sends
optimistically locked patch operations instead of mutating local task state.

## Verification

Completed on 2026-07-21:

- `echo-agent`: formatting, strict Clippy/panic API gates, all-feature workspace
  tests, no-default-feature libraries, and the isolated feature matrix.
- `echo-agent-cli`: formatting, all-target/all-feature Clippy, all-feature
  workspace tests, no-default-feature application core, GUI-only check and
  tests.
- `web-frontend`: Prettier, 20 Vitest files / 77 tests, and production build.
- Regression coverage includes atomic plan creation, optimistic conflicts,
  capability rejection, blocked-task restoration, latest-revision completion,
  and safe-point dispatch of a task inserted by a newer revision.
