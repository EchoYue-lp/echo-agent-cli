# Runtime DAG Kernel Convergence

Date: 2026-07-27

## Decision

Dynamic Agent plans use one framework-owned DAG execution loop. EKO supplies a
controller/dispatcher adapter for product policy and persistence; it does not
own a second ready-frontier loop.

The staged convergence is complete after a correctness re-open. Framework DAG
traversal, structural validation, task status, immutable-spec/mutable-execution
semantics, and revision-safe dispatch claims now have one authority. EKO
retains product persistence/UI projections, product resource limits, and a
controller/dispatcher adapter.

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

The Codex manual helper was retried after network access was restored, but the
official endpoint still returned HTTP 403. The decision was therefore
cross-checked against the mature implementations already vendored in this
workspace and the previously captured Claude Code/Codex lifecycle behavior:

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
| `TaskSpec` + `TaskExecution` + shared `TaskStatus` | `TaskPlan` file/event projection |
| Structural `PlanValidator` | Subagent/tool capability validation |
| Revision safe-point reload and atomic task claim | `DomainProfile` and role routing |
| Ready frontier and dependency blocking | Review and acceptance policy |
| `max_concurrent_subagents` wave bound | Writer/shell/LLM limits in `EkoExecutionLimits` |
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

## Phase 2 Completed

- Replaced the flat framework `RuntimeTask` shape with an immutable
  `RuntimeTaskSpec` plus mutable `RuntimeTaskExecution` composition.
- Preserved `required_artifacts`, executable `execution_checks`, and semantic
  `acceptance_criteria` as distinct fields; removed the lossy verification-list
  flattening from `PlanTask::to_runtime_task()`.
- Added the generic metadata extension point and mapped only EKO-specific
  `DomainProfile`, `parallel_group`, and `sort_order` through
  `EkoTaskMetadata`.
- Extended the existing framework `PlanValidator` to validate revisioned
  runtime specs/snapshots: identity, duplicates, dependency existence, cycles,
  depth, retry bounds, and spec/execution identity.
- Deleted EKO's duplicate dependency/DFS validator. EKO retains only catalog
  checks (Subagent roles/tools) and file-ownership policy.

## Phase 3 Completed

- `PlanSpec` now compiles its hard dependencies into canonical
  `RuntimeTaskSpec` values. Preferred and optional edges remain authoring
  policy and do not silently become runtime blockers; all edge endpoints are
  still validated.
- `PlanValidator::validate(PlanSpec)` delegates identity, dependency, cycle,
  depth, and retry checks to `validate_runtime_specs`. `PlanSpec` and
  `TaskManager` topological-order queries call the same canonical topology
  implementation instead of maintaining their own Kahn loops.
- Rich framework `Task` records expose a thin immutable-spec/mutable-execution
  projection for the runtime kernel. Authoring-only structured fields remain
  available in metadata instead of being flattened into acceptance checks.
- Framework `TaskExecutor::execute_all` now delegates full traversal to
  `RuntimeDagExecutor`. Its controller only loads `TaskManager` snapshots,
  selects an existing scheduler policy, invokes the established per-task
  pipeline, persists outcomes, and maps status back.
- Hooks, verifier, replanner, TaskStore, per-task timeout/retry, scheduler, and
  background one-wave APIs remain valid framework capabilities. The previous
  `execute_all` ready/deadlock loop and its round-timeout configuration were
  removed.
- Snapshot cancellation is a first-class runtime outcome, pending downstream
  tasks can transition directly to Blocked after an upstream failure, and a
  zero task timeout now correctly means no timeout.

## Phase 4 Completed

- Renamed the framework's canonical runtime model to `TaskSpec`,
  `TaskExecution`, `TaskStatus`, `TaskKind`, and `Task`. The previous
  `RuntimeTask*` names no longer coexist with another public task model.
- Renamed the rich hooks/verifier/attempt/store record to `ManagedTask`. It
  projects into canonical `Task` and does not own DAG traversal or structural
  validation.
- Renamed the LLM authoring artifact to `PlanTaskSpec`, so it cannot be
  confused with the immutable runtime `TaskSpec`.
- Merged the former managed/runtime status enums. `ManagedTask` and the DAG
  kernel now use the same `TaskStatus`, including failure, blocked, timeout,
  retry, pause, and cancellation detail.
- Renamed EKO's Rust persistence/UI DTOs to `EkoTaskSpec` and
  `EkoTaskExecution` while preserving their existing serialized and
  TypeScript wire names. They are explicitly projections, not scheduler
  authorities.
- Added a checked `framework Task -> PlanTask` conversion. Unsupported EKO
  status projections, mismatched spec/execution identities, invalid metadata,
  and conflicting failure details return errors instead of being flattened.
- Removed the last EKO-specific historical comment from generic framework task
  checkpoint code.

## Correctness Re-open Completed

- Added `TaskClaim { revision, attempt, spec_hash }`. The kernel acquires a
  Subagent permit, atomically claims an exact Pending task revision, and only
  then dispatches. Revision/status/spec conflicts return `ReloadSnapshot`; they
  are never converted into task failures.
- Every completion, failure, block, and retry write carries the claim. EKO
  applies it only while the same claim is still Running; late results return
  `Superseded` and cannot overwrite cancellation, retry, or a changed plan.
- Subagent execution identity is now
  `{task_id}:{plan_revision}:{attempt}`. Durable result lookup therefore cannot
  reuse output produced for an older TaskSpec when a plan patch changes the
  specification without increasing `retry_count`.
- `run-state.json` persists the shared `TaskStatus` separately from
  `failure_fingerprint`. Failed, timed-out, and blocked details survive restart
  and feed terminal outcomes instead of degrading to generic titles or hashes.
- Removed EKO write/shell/LLM limits from the framework API. The generic kernel
  exposes only `max_concurrent_subagents`; EKO owns `EkoExecutionLimits` and
  consumes its remaining semaphores in the application adapter.
- Removed panic-prone `unwrap`, `expect`, direct indexing, and explicit panic
  branches from the touched EKO executor tests.

## Remaining Work

No duplicate DAG executor, structural validator, runtime status model,
canonical task-spec name, or unclaimed dispatch path remains. Operational
follow-up is limited to observing real long-running GUI/TUI/CLI/channel runs for
claim conflicts, safe-point reload, cancellation, durable Subagent reuse, and
worktree finalization.

## Verification

- Framework runtime executor unit tests cover dependency order, safe-point plan
  revision, claim-conflict reload, skipped tasks, downstream blocking, and
  persisted Failed/TimedOut/Blocked details.
- Framework `PlanValidator` tests cover acyclic runtime specs, dangling
  dependencies (including non-blocking authoring edges), cycles,
  spec/execution identity mismatch, and authoring-field preservation.
- Framework `TaskExecutor` tests cover full-kernel dependency order, failure
  propagation, cancellation, disabled timeout, UTF-8-safe context projection,
  and the retained per-task execution capabilities.
- Framework and EKO adapter tests cover shared status-detail preservation,
  fingerprint separation, separated execution checks/acceptance criteria, EKO
  metadata, and checked round trips through canonical `Task`.
- EKO store tests cover a plan patch winning before claim, stale completion
  after cancellation, and changed TaskSpec output receiving a new execution id
  without a retry-count bump.
- All 44 EKO executor tests pass, including durable result reuse, revision
  insertion, cancellation, review outcomes, sibling completion, worktree merge
  failure, and external in-flight observation.
