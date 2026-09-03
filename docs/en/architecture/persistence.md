# EKO Persistence Architecture

EKO uses file persistence under `~/.eko` (or `EKO_DATA_DIR`) and does not
enable SQLite in the application. Framework store implementations remain
available to other framework consumers.

## Layout

```text
~/.eko/
  config.yaml
  mcp.json
  hooks.yaml
  skills/
  enabled-skills.json
  plugins/
  workspaces/
    <workspace-id>/
      .eko/
        workspace.json
        sessions/
        conversations/
        memory/
        evolution/
        tasks/
        traces/
        artifacts/
        uploads/
        data/
        papers/
        logs/
```

All paths are resolved through `echo_agent::paths` or `WorkspaceLayout`. The
application does not scan or import `~/.echo-agent`, and it does not construct
`SqliteStore` or `SqliteConversationStore`.
`.eko/workspace.json` is the only readable workspace manifest. A root
`.workspace.json` is checked only to prevent accidental overwrite during
workspace creation; it is never parsed or migrated.

## Authorities

- Framework `ConversationStore` owns the durable conversation transcript.
- Framework runtime state owns the compact checkpoint needed to resume an
  in-flight turn.
- EKO `ChatEventLog` owns product event payloads, conversation identity,
  retention pins, and UI/channel projection while reusing the framework
  segmented journal primitive.
- EKO `TaskRuntimeStore` owns the file-backed event journal and projections for
  TaskRun, PlanRevision, Todo, recovery, and workspace supervision.
- Every store-backed turn eagerly owns a TaskRun. Typed execution provenance
  distinguishes an internal conversation-turn journal from an orchestrated run;
  a conversation run enters the task UI only after it commits a plan.
- Tool invocation and artifact projections retain typed framework
  `ToolResult.artifact` values; local paths are never treated as terminal truth.

These authorities are complementary. A checkpoint is not a transcript, a Todo
projection is not a task graph, and a frontend store is not durable state.
`ChatEventLog` owns surface delivery/replay while TaskRuntime owns the associated
Goal, user constraints, execution, and recovery facts. See
[ADR 0037](../adr/0037-unified-turn-run-binding.md).

## Recovery and Retention

Boot reconciliation closes interrupted command cells and repairs projections
from the authoritative journal. Segment and cursor projections may be pruned
and rebuilt; pruning does not create a second source of truth. Workspace and
conversation deletion operate on complete scopes and retain a typed repair debt
when a durable fact succeeds but projection cleanup is temporarily degraded.

The application protects user and run data by ownership and lifecycle, not by
file extension. Runtime traces, artifacts, journals, and release evidence are
removed only as an explicitly resolved scope after active owners, recovery
debt, and references have been checked.

See [ADR 0006](../adr/0006-scoped-product-data-io.md),
[ADR 0008](../adr/0008-taskruntime-bounded-query-projections.md),
[ADR 0011](../adr/0011-boot-inbox-recovery-authority.md), and
[ADR 0018](../adr/0018-typed-tool-artifact-projection.md).
