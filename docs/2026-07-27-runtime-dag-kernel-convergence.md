# Runtime DAG Kernel Convergence

Date: 2026-07-27

## Decision

Dynamic Agent plans use one framework-owned DAG execution loop. EKO supplies a
controller/dispatcher adapter for product policy and persistence; it does not
own a second ready-frontier loop.

This is a staged convergence. The executor authority has moved, but the older
framework `Task`/`PlanSpec` model and EKO `TaskPlan`/`PlanTask` model still
overlap. That remaining model and validator convergence is the next phase, not
a reason to retain two executors.

## Evidence Before The Change

- `echo-orchestration::tasks::TaskExecutor` implemented dependency traversal,
  concurrency, retry, cancellation, and deadlock handling for the older
  framework `Task` model.
- EKO implemented another complete dynamic loop in
  `task_runtime/executor.rs::run_dag`.
- EKO already converted every `PlanTask` into the framework `RuntimeTask` view
  and reused `DagExecutionState`, proving that a product-neutral kernel existed
  but stopped at bookkeeping.
- EKO-specific review, worktree, event, and file-store behavior was interleaved
  with the duplicate traversal loop, making the ownership boundary unclear.

## Reference Implementations

Online official documentation could not be re-fetched in this environment: the
web endpoint returned 404/403 and browser policy blocked the official Claude
Code and OpenAI documentation domains. The decision was therefore cross-checked
against the mature implementations already vendored in this workspace:

- OpenCode keeps Subagent session/background-job mechanics behind the task tool
  and services, while the tool adapts product permissions and parent-session
  projection: `opencode/packages/opencode/src/tool/task.ts`.
- DeepAgents keeps async Subagent run lifecycle in middleware and projects a
  stable task id separately from the current run id:
  `agent-fram/deepagents/libs/deepagents/deepagents/middleware/async_subagents.py`.
- Hermes Kanban centralizes readiness, claiming, completion, and dependency
  updates in its task authority, while gateway/dashboard code consumes that
  lifecycle: `hermes-agent/hermes_cli/kanban_db.py`.

The common pattern is one lifecycle/execution authority plus thin product or
surface adapters. Review UI, worktree policy, and domain routing are not part of
the generic frontier loop.

## Ownership Boundary

| Framework (`echo-agent`) | EKO (`echo-agent-cli`) |
|---|---|
| Runtime task view and status | `TaskPlan` file/event projection |
| Revision safe-point reload | `DomainProfile` and role routing |
| Ready frontier and dependency blocking | Review and acceptance policy |
| Bounded Subagent waves | Writer/shell/LLM resource limits |
| Parent cancellation and join cleanup | Worktree and file ownership policy |
| External in-flight observation | Durable Subagent result recovery |
| Stall/deadlock outcome | GUI/TUI/CLI/channel event mapping |

The adapter may select a product-safe subset of the ready frontier and resolve
a dispatch into Completed/Pending/Failed/Blocked. It may not implement another
DAG loop, dependency validator, or generic retry state machine.

## Phase 1 Completed

- Added `RuntimeDagExecutor`, `RuntimeDagController`, coherent revision
  snapshots, generic task resolutions, and terminal outcomes to
  `echo-orchestration`.
- Moved revision reload, ready-frontier traversal, global Subagent concurrency,
  wave joining, cancellation, failure propagation, external in-flight polling,
  and stall detection into the framework.
- Replaced EKO's 596-line scheduling loop with `EkoRuntimeDagController` and a
  thin `execute_runtime_plan` adapter.
- Kept review, persistence, worktree integration, file locks, and attended vs
  unattended stop policy in EKO.
- Fixed skipped-plan nodes so they count as deliberately resolved instead of
  producing a false DAG stall.

## Remaining Convergence

1. Split the framework runtime model into immutable task specification and
   mutable task execution without creating a third model.
2. Map EKO's separate execution checks, acceptance criteria, and required
   artifacts without flattening them into one verification field.
3. Consolidate duplicate plan/DAG validation into the framework validator with
   EKO metadata validation supplied as product policy.
4. Route the older framework `TaskExecutor` through the same runtime kernel, or
   delete the old mechanism only after the replacement covers its hooks,
   verifier, replanner, timeout, and public framework use cases.
5. Remove obsolete EKO-labelled fields/comments from generic framework APIs
   only when their generic meaning or replacement is clear.

## Verification

- Framework runtime executor unit tests cover dependency order, safe-point plan
  revision, skipped tasks, downstream blocking, and terminal outcomes.
- All 44 EKO executor tests pass, including durable result reuse, revision
  insertion, cancellation, review outcomes, sibling completion, worktree merge
  failure, and external in-flight observation.
