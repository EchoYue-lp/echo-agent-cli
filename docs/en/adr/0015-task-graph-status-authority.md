# ADR 0015: Task Graph Status Authority

- Status: Accepted
- Date: 2026-08-28

## Context

EKO already used the framework `TaskRevisionService`, `RuntimeTaskService`,
`PlanValidator`, and ready frontier as the only revisioned DAG authority. The
application nevertheless materialized every `PlanTask` with `TodoStatus` plus
an optional `status_detail`, converted that smaller status back into framework
`TaskStatus`, and exposed the joined result as a generated `TaskPlan` DTO.
Completion, recovery, retry, and some dependency consumers could therefore
read a lossy UI projection instead of the canonical execution state.

The repository-wide search covered both `echo-agent` and `echo-agent-cli` for
`TaskStatus`, `TodoStatus`, `PlanTask`, `TaskPlan`, `TodoUpdated`, validators,
ready-frontier loops, revision services, task stores, completion gates, and
surface reducers. The framework has one validator, revision service, runtime
service, and ready frontier. EKO has one TaskRuntime journal and checkpoint
fold. The duplicated semantics were the application-side Todo status field,
its reverse conversion, the `TodoUpdated` status event, and the generated plan
DTO carrying execution fields.

The approved interaction-convergence plan is based on Codex's separation of
Thread, Turn, Item, and task lifecycle, and Claude Code's separation of task
artifacts from execution handles. EKO keeps the cross-system result: a plan is
an editable artifact, execution has one typed lifecycle, and a Todo list is a
display projection. It does not add Claude Code `TodoWrite`, `TaskOutput`, or
`TaskStop` equivalents, and it does not add a Codex-style second mailbox or
task store. See [Agent collaboration ADR](./0001-agent-collaboration.md),
[Codex capability catalog](./0002-codex-tool-capability-catalog.md), and
[Claude Code capability catalog](./0003-claude-code-capability-catalog.md).

## Options

1. Keep `TodoStatus` on materialized plan nodes and harden conversions.
   Rejected because richer `Retrying` and `Paused` states remain lossy and
   runtime consumers can still select the projection accidentally.
2. Add an EKO-specific execution status enum matching framework states.
   Rejected because it creates a second state machine and conversion owner.
3. Keep framework `TaskStatus` as the only execution state, persist immutable
   `PlanRevision` specifications separately from canonical execution state,
   and derive `TodoItem` only at the query boundary. Selected.

## Decision

### Layering

- **Framework mechanism**: `TaskStatus`, `TaskSpec`, `TaskExecution`,
  `TaskRevisionService`, `RuntimeTaskService`, `PlanValidator`, dependency
  traversal, retry, cancellation, and the ready frontier remain in
  `echo-agent`. F2 adds no framework runtime or validator.
- **EKO product policy**: Goal, review, evidence, worktree, Subagent role,
  artifacts, and the TaskRuntime journal remain in `echo-agent-cli`.
  `EkoTaskSpec` is an application extension of framework `TaskSpec`;
  `RunStateSnapshot.tasks` stores framework `TaskExecution` without flattening
  status detail.
- **Projection boundary**: the internal materialized `PlanTask` is a transient
  spec/execution join and carries canonical `TaskStatus`. It is neither a
  persisted file contract nor an IPC DTO. `PlanRevision` is the plan artifact
  returned to surfaces. `TodoItem` is derived from the same journal fold and
  adds only display metadata and retry counts.

`TodoStatus` has one direction only: `TaskStatus -> TodoStatus`. There is no
Todo-to-task conversion. Completion, dependency summaries, requirement skip,
retry eligibility, recovery blockers, and terminal checks read canonical task
execution state. Surface code reads `PlanRevision` for immutable task fields
and `TodoItem` for display state.

Task execution journal events retain the established typed terminal event
names. Pending, retrying, and paused transitions use `TaskStatusChanged` rather
than `TodoUpdated`; the payload preserves the canonical status name and detail.
The event fold reconstructs `TaskStatus` first and builds Todos afterward.

The task tools remain `task_create`, `task_update`, `task_list`, and
`task_execute`. No TaskGet, TodoWrite, workflow store, second reducer, or
surface-local state mutation is introduced. The former interaction-mode surface
was removed by F4; task graph execution remains explicit through `task_execute`.

### Physical settlement and lossless execution facts

The framework's exact-claim interruption request is persisted before signalling
the original cancellation token. Submission does not settle the claim or emit a
TaskRun terminal. The eventual framework CAS receipt determines the committed
status, including a stronger interruption that wins over a late dispatch result.

The internal spec/execution join and discardable checkpoint retain the complete
canonical `TaskExecution`, including an absent versus explicit-null result and
`interruption_requested`. Graph commits and execution events preserve those
facts in the same journal. Older query checkpoints are replayed from that
journal; `TaskSpec` and `TodoItem` IPC remain unchanged. An unresolved original
physical-owner intent from the current process blocks boot recovery and duplicate execution rather than
being converted into another Task status.


## Consequences

- Retrying and paused execution survive restart without being flattened to
  running or pending and reconstructed from display text.
- Removing plan execution fields from generated TypeScript cannot change DAG
  replay because those fields were never the journal authority.
- GUI progress and retry controls combine immutable plan specifications with
  the read-only Todo projection instead of treating the plan response as a
  mutable status store.
- Existing TaskRuntime files remain split into `plan.json` specification and
  `run-state.json` execution projections, both rebuildable from one journal.
- Future task lifecycle states must be added to framework `TaskStatus` first;
  Todo may deliberately collapse them for display but can never write back.

Boot recovery treats a valid unresolved current-process physical-owner intent as a disposition of that exact run: under its original run lock, leave the run, command cells and pending Subagent control facts unchanged and continue recovering unrelated historical runs. Previous-process admission fences are retired only under the exclusive root authority as specified in [ADR 0049](0049-taskruntime-process-owner-recovery.md), before the existing interruption recovery classifies effects. Malformed intent facts and storage failures still fail recovery. The same original evidence reader and exact physical acknowledgement path serve recovery and per-run admission; this does not authorize replay or synthesize a terminal receipt. A continuation consumes the canonical existing run domain, route, attended mode, Goal and execution profile; its internal instruction is not a new user goal. The actual active workspace and caller conversation/root still pass through the original immutable-identity validator, including negative mismatch checks; RunStarted projection uses the returned original goal/route.


Continuation progress excludes only private physical_owner_intent metadata on RecoveryBlocked/RecoveryResolved from both the existing progress predicate and fingerprint. Real business recovery and task events still count. The original explicit-resume Goal/revision and no-progress limits remain unchanged. Canonical before evidence excluded an unconfirmed-owner fence replay: each original owner completed and was acknowledged before the next turn; the defect was metadata resetting the business-progress budget. The same canonical journal remains the only authority.
