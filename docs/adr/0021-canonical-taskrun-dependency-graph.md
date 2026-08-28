# ADR 0021: Canonical TaskRun dependency graph

## Status

Accepted, 2026-08-29.

## Context

EKO already has one revisioned dependency authority: `TaskRun -> PlanTask`, with
`PlanRevision.tasks[].depends_on`, framework validation, claim settlement, and Todo projection.
The legacy background adapter also accepted `depends_on` as TaskRun IDs, persisted those IDs in
free-form trigger metadata, polled other runs every 250 ms, and exposed an unscoped global DAG.
That path had no revision, cycle validation, exact workspace identity, or explicit external-workflow
contract.

Mature systems keep these meanings explicit:

- [Claude Code Agent Teams](https://code.claude.com/docs/en/agent-teams) coordinates dependent work
  through one shared task list; unresolved dependencies block claiming within that list.
- [GitHub Actions `jobs.<job_id>.needs`](https://docs.github.com/en/actions/reference/workflows-and-actions/workflow-syntax#jobsjob_idneeds)
  relates jobs inside one workflow run.
- [Temporal Child Workflows](https://docs.temporal.io/child-workflows) models cross-workflow ownership
  as an explicit parent/child operation with its own lifecycle policy.
- [Airflow `ExternalTaskSensor`](https://airflow.apache.org/docs/apache-airflow-providers-standard/stable/_api/airflow/providers/standard/sensors/external_task/index.html)
  makes external-DAG waiting an explicit sensor, not an ordinary task dependency field.

## Decision

EKO has one task dependency API: `PlanRevision.tasks[].depends_on` inside an explicitly selected
TaskRun. `TaskRevisionService` and `RuntimeTaskService` remain the only relation and execution
authorities.

The background launcher no longer accepts or persists cross-TaskRun `depends_on`, reconstructs a
metadata graph, or polls another TaskRun. Its launch receipt identifies `workspace_id` and `run_id`.
CLI graph display requires `/tasks dag <run-id>` and renders the existing plan/todo projections.
The unscoped Tauri `get_task_dag` command is removed.

If EKO later needs cross-run orchestration, it must be a separately reviewed explicit product type
with workspace-qualified identity, lifecycle ownership, recovery, and cancellation semantics. It
must not reuse `PlanTask.depends_on` or return as a hidden launcher fallback.

## Alternatives

### Keep trigger metadata and polling

Rejected. It creates a second relation store and an unversioned scheduler outside the canonical
TaskRun graph.

### Add a generic framework cross-run edge

Rejected. EKO has no proven reusable contract for external run ownership, and adding one would
pollute the framework with product policy.

### Remove dependency capability entirely

Rejected. Same-run dependency planning remains fully supported through `task_create`,
`task_update`, `task_list`, `task_execute`, PlanRevision, and Todo projection.

## Consequences

- GUI, CLI, TUI, JSONL, and channel retain the same canonical PlanTask dependency capability.
- Background launch is immediate and cannot be stranded in a hidden 250 ms polling loop.
- A run graph is always selected by workspace and run identity before its edges are rendered.
- Existing trigger events may contain obsolete fields, but current code neither reads nor writes
  them; the project has no compatibility migration requirement.
