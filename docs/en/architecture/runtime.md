# EKO Runtime Architecture

This page records runtime ownership, lifecycle, workspace execution, and task
coordination. It is the current behavior reference; historical alternatives
remain in ADRs.

## AgentRouter Migration Handoff

The AgentRouter uses the public typed
`echo_agent::delivery::DeliveryLedger<Journal, AgentAddress, AgentMessage>`
directly. Its journal, checkpoint, claim, cursor, settlement, FIFO, retry and
retention state are the framework's typed records; there is no app-side
projection/reducer or wire conversion in the active path.
AgentRouter passes the framework `DeliveryTransition` directly and commits its
validated event through `DeliveryLedger::apply_prepared_with`; the application
callback is limited to physical journal durability and reopen handling. There
is no EKO-side lifecycle command enum.
`AppState` still owns EKO endpoint
validation, live/cold runtime selection, wake scheduling, groups, retirement,
and surface policy. Existing local inbox data is intentionally discarded after
this development-time schema change.

EKO's persisted `TaskExecutionSummary` and `SuggestedTask` records remain
application-owned because they carry review, task-kind, and surface fields.
They are stored directly in EKO; no unused `to_runtime_*` or
`from_runtime_*` result conversion API is kept.

`EkoConfig` likewise remains the product file schema. Bootstrap selects its
provider-neutral fields for the framework runtime configuration; EKO does not
add a second configuration conversion wrapper. Framework callers convert
`FrameworkConfig` with the standard `From<FrameworkConfig> for AgentConfig`
contract.

`TaskExecutionSummary.outcome` and `SubagentRun.outcome` are the durable EKO
fields for a typed subagent outcome. The framework's `SubagentResult` remains
the execution envelope for output, timing, usage, and mode; EKO does not copy
it into a second result DTO.
`ConversationInputOutcome` remains only as an EKO wire name; Rust uses the
framework `AgentSteerTurnOutcome` directly, without per-variant outcome
conversion.
`ChatSteerOutcome` is likewise only a GUI wire name and reuses that framework
outcome directly in Rust.
`SubagentRun.usage` stores framework `ExecutionUsage` directly;
`SubagentRunUsage` remains only the generated TypeScript wire name. The executor
no longer wraps framework `SubagentResult` or `TurnReceipt` values in a
`TaskExecutionUsage`; both result surfaces expose `ExecutionUsage` through
their direct `usage()` API.
EKO command receipts likewise alias framework `SubagentCommandIdentity` rather
than defining a second generic identity model.
Their durable phase aliases framework `SubagentCommandPhase`; only the EKO
status label (`Pending/Accepted/Rejected/Settled`) remains a surface projection.

Permission state follows the same framework-native rule: `ConfigState` stores
`echo_agent::tools::permission::PermissionRule` directly. Tauri parses its
matcher, behavior, and source strings through the framework `FromStr` APIs and
does not reconstruct an application `PermissionRuleConfig`.

EKO task specifications use the framework's `TaskSpec::with_extension` and
`TaskSpec::extension_as` helpers for the complete typed `EkoTaskExtension`.
Only partial `TaskPatch` updates retain a JSON object because they describe a
subset of fields rather than a complete extension value. Crossing the EKO
projection boundary uses standard `TryFrom`/`TryInto`; no source-named
`to_task_*` or `from_task_*` API is exposed.

## Process Owners

`AgentRuntime::bootstrap` creates the shared model, HITL, prompt, MCP, Plugin,
Browser, and base Agent resources. GUI, TUI, CLI, and channels use the same
app-core services and shutdown order.

`ApplicationLifecycleOwner` takes ownership after bootstrap succeeds. Shutdown
closes admissions, broadcasts cancellation, and waits for accepted foreground,
TaskRun, Agent delivery, pool, and background work. GUI and headless failures
return the same typed aggregate receipt; bootstrap rollback uses the same owner
instead of maintaining a second resource list. See [ADR 0004](../adr/0004-application-lifecycle-supervisor.md).

The process-level services are:

- `ForegroundTurnControl` for foreground admission, exact cancellation, and
  typed settlement.
- `AgentRouter` for workspace/conversation endpoints, durable inboxes, and
  Agent groups.
- `AgentControlService` as a routing adapter for `ConversationTarget` and
  `TaskSubagentTarget`; it delegates message, follow-up, wait, and interrupt
  without owning a mailbox, TaskRun graph, retry loop, or terminal reducer.
- `PluginRuntimeService` for prepared framework generations, EKO target
  publication, preferences, and product policy.
- `McpConfigRuntime`, `BrowserRuntime`, and `ExtensionControlService` for
  product-owned configuration, browser events, and extension mutation policy.
- `WorkflowService` and `StructuredExtractionService` for EKO catalog and
  surface adapters while framework owns graph and extraction execution.
- `SubagentEnvelopeProjectionService` for the single shared framework
  `SubagentEventBus` subscription, exact EKO addressing, ChatEventLog commit,
  gap reconciliation, and surface-neutral live publication.

### Subagent execution projection

All bootstrap, pooled conversation, and workspace Agents share one framework
`SubagentEventBus`. App-core consumes only its versioned envelope stream. It
resolves formal events through the TaskRuntime owner registry and run-less
events through an exact active foreground turn; current workspace focus and
execution-id string formats are never identity inputs.

The process Subagent admission is installed during Agent construction or
post-bootstrap task-tool registration. TaskRuntime does not reacquire an Agent
write lease from inside `task_execute`, which may already run under that Agent's
outer ReAct write owner.

The app-core projector retains framework event metadata on generated
`ExecEvent`, appends the event to the existing `ChatEventLog`, and invokes the
existing tool detail projector. Sequence jumps use framework replay; an
unrecoverable transient suffix becomes `subagent_stream_gap`, while retained
terminal data still reconciles the final outcome. Recovery scans retained,
active, and known streams; an active publisher's dispatch-start anchor preserves
address identity after full replay eviction. Tool detail failures are retried
from the already-committed event and remain rebuildable from ChatEventLog.
Active GUI/TUI/CLI/channel
sinks receive the committed event through the journal's weak live registry;
background GUI, TUI, and REPL delivery uses the committed projection stream and
its bounded late-subscriber/lag replay. Request-scoped channels replay execution
events from the durable conversation cursor on their next response. Live and
replay therefore share the same `ChatDriverEvent::Execution` payload.

Tauri and TUI do not subscribe to raw Subagent events. Tauri publishes committed
chat envelopes and tool summaries only. See [ADR 0040](../adr/0040-app-core-subagent-event-projection.md).

## Workspace Runtime

`WorkspaceRuntimeRegistry` is the process owner for loaded workspace hosts.
Each `WorkspaceRuntimeHost` binds one immutable workspace ID and root and
prepares the file resources for conversations, runtime state, memory, deletion,
and TaskRuntime.

`WorkspaceExecutionRuntime` is created lazily inside a host. It retains the
primary Agent, AgentPool, TaskRuntime, review integration, and Plugin/MCP
receipts for that workspace. Changing GUI focus never rebinds an accepted run.

Workspace operations carry `workspace_id` and a generation derived from the
creation timestamp and project-root revision. A scoped workspace control holds
the same host incarnation through settlement. Product data I/O uses the shared
`ProductDataIoService`, and cleanup keeps its owner until deletion has settled.

## Conversation and Delivery

The application enforces at most one foreground turn for a workspace
conversation. A turn captures its workspace runtime once and never reads UI
focus again during execution. Framework conversation transcript and
incarnation-scoped runtime checkpoint remain separate authorities; frontend
stores are rebuildable projections.

Cross-session delivery and command-cell-watch handoff use framework tracked steer receipts.
Mailbox acceptance is not consumption: durable delivery facts are written
before side effects, and replay is forbidden when owner loss has no typed
terminal. Conversation and workspace deletion use retirement guards to clean
the matching inbox.

`watch_cell` acquires the framework `CommandCellWatcher` before EKO validates
the durable TaskRuntime or chat owner. The deterministic watcher holds the
retention lease, drains the retry-safe byte cursor, and publishes only typed
terminal truth. It performs no model call and consumes no Subagent capacity.

Long-running TaskRun continuation uses one foreground root owner and advances
the active turn ID through a progress handle. Cancellation addresses the root;
steer addresses the active turn. TaskRun resume validates durable run identity
and current `turn_id`.

## Task Runtime

Framework `RuntimeTaskService` drives the production DAG. EKO supplies product
policy, file-journal transactions, review/worktree behavior, and surface
adapters. `PlanRevision` is the editable artifact; `TaskStatus` is execution
authority; Todo is read-only projection.

TaskRuntime events are authoritative and run/plan/todo/outcome/checkpoint data
are recoverable projections. Claims, revisions, attempts, and Subagent outcomes
carry stable identities. Store-owned operation supervisors provide bounded
async and blocking I/O, and shutdown/workspace eviction waits for accepted
operations rather than caller future lifetime.

See [ADR 0005](../adr/0005-foreground-continuation-owner.md),
[ADR 0009](../adr/0009-taskruntime-async-io-and-ipc-boundary.md), and
[ADR 0015](../adr/0015-task-graph-status-authority.md).
