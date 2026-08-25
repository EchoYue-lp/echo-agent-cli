# ADR 0006: Workspace-scoped product-data I/O

## Status

Accepted.

## Context

EKO's file browser, research library, and analysis workbench are product
features owned by `echo-agent-cli`. Their Tauri commands previously resolved
the currently focused workspace after command admission, and several commands
performed synchronous filesystem work directly on Tokio workers. A focus
change could therefore retarget a command, a deleted/recreated workspace could
reuse the same textual ID, and slow filesystem operations could stall unrelated
async work.

Automatic research ingestion adds a second cancellation boundary: the
provider tool completes asynchronously, then EKO persists its result on the
blocking pool. Framework tool execution and Subagent dispatch may spawn new
tasks, so a Tokio task-local or a root inferred from a long-lived Agent cannot
prove that the admitted workspace still exists when that blocking closure
finishes.

The workspace runtime already provides the required authority:
`AppState::product_data_for_scope` returns a `ScopedProductData` backed by a
`ScopedWorkspaceControl` whose control lease pins the exact host until every
non-abortable closure is settled. The
workspace registry already persists `Workspace.created_at` and owns linked
project metadata. A monotonic `project_root_revision` in that same metadata
identifies both workspace creation and project-root incarnations without
another generation store.

Relevant mature-runtime guidance:

- [Tokio `spawn_blocking`](https://docs.rs/tokio/latest/tokio/task/fn.spawn_blocking.html)
  requires blocking work to leave async workers, notes that started blocking
  tasks cannot be aborted, and recommends limiting parallel work where needed.
- [Tokio graceful shutdown](https://tokio.rs/tokio/topics/shutdown) combines an
  explicit cancellation/admission signal with a tracker that waits for owned
  tasks to finish. A process-global task owner is therefore not equivalent to
  an application-generation owner when several AppState instances overlap or
  restart in one process.
- [`TaskTracker::close` and `wait`](https://docs.rs/tokio-util/latest/tokio_util/task/task_tracker/struct.TaskTracker.html)
  model the same two-phase contract used here: close one owner's admission,
  then wait until every task admitted by that owner has exited.
- [React `useEffect`](https://react.dev/reference/react/useEffect#fetching-data-with-effects)
  documents ignoring obsolete network responses to avoid request races.
- [Tauri commands](https://v2.tauri.app/develop/calling-rust/) support explicit
  command arguments and managed state, allowing the frontend scope to be
  validated before work begins.

## Options considered

### Resolve the current workspace inside every command

Rejected. Focus is a UI projection, not an execution identity. Commands that
wait during a transition can land in another workspace.

### Derive workspace identity from a root hash

Rejected. A hash duplicates registry identity, leaks path semantics into API
contracts, and still does not model deletion/recreation cleanly.

### Add separate file/research/analysis stores per surface

Rejected. The domain modules and `ScopedWorkspaceControl` already exist. New
stores would create duplicate persistence and lifecycle authorities.

### Explicit scope plus the existing control lease

Accepted. Every GUI request carries `workspace_id` and
`workspace_generation`; the latter combines the registered workspace's
`created_at` value with its `project_root_revision` (`global` for global scope).
The backend resolves one
`ScopedProductData`, validates the generation, and retains it until the
operation settles.

### Infer automatic-ingest ownership from `ToolContext.working_dir`

Rejected. A working directory is a path, not an ownership receipt. It may also
be replaced by Subagent isolation. Path presence alone cannot keep deletion or
project relinking busy after an outer waiter is cancelled.

## Decision

1. Research and analysis data use
   `ScopedWorkspaceControl::data_root()`, which is exactly
   `execution_scope.root()`.
2. Project files use `ScopedWorkspaceControl::project_root()`. A linked
   project changes the file-browser root without moving EKO-owned research or
   analysis data.
3. Synchronous file, research, and analysis I/O enters one cloneable
   `ProductDataIoService` created by `AgentRuntime` for the current application
   generation. `AppState`, CommandCell, analytics, workspace recovery and
   Agent-owned research tools receive that same service. The process-wide
   semaphore limits aggregate blocking concurrency, but it owns no lifecycle.
   `ApplicationLifecycleOwner` seals this generation's product-data admission
   in phase one together with top-level producer admission. New standalone I/O
   and new flows are rejected immediately. There is no production free-run
   adapter or static operation owner.
4. Every accepted multi-stage producer obtains one cloneable async-flow receipt
   before its first provider/transform await. Manual compression, aggregate
   deletion/recovery, analysis/analytics, research/AutoIngest, CommandCell,
   workspace preparation and channel attachment preparation use only that
   receipt's nested-I/O token afterward. Nested I/O stays legal after phase-one
   seal until the flow publishes stable settlement. Dropping the surface waiter
   cannot cancel the existing subsystem owner; durable failure remains typed
   shutdown debt (and deletion also retains its retryable tombstone).
5. Tauri closures capture the scoped control value, so workspace deletion is
   busy until non-abortable blocking work settles.
6. Analysis execution acquires its Agent from the scoped runtime. Cancellation
   identity is the pair `(workspace_id, analysis_id)`. An app-owned
   `AnalysisRunSupervisor` retains the exact control receipt and JoinHandle;
   `run` returns immediately, while `wait` reports Started/Joined for the same
   receipt and `cancel` uses the framework draining-started tool API before
   joining cleanup. Successful join removes active ownership immediately and
   places only the result in a bounded completed-receipt cache, so abandoned
   callers cannot keep a workspace busy.
7. CLI, TUI, GUI, and channel commands use the same `ScopedProductData` and
   shared analysis/research command catalogs; they never infer roots from a
   long-lived Agent. Analysis save accepts the full typed request (title,
   script, expected revision, inputs, parameters, and random seed), and run,
   edit, and delete acquire one atomic cross-surface owner.
8. Frontend requests carry both scope fields. File state rejects mismatched
   responses; workspace-incarnation keys remount research/analysis/file panels,
   and long-lived panel requests check the captured scope before publishing.
9. Agent-owned product-data work uses a cloneable `ScopedWorkspaceIoReceipt`
   containing only the exact runtime lifetime and immutable EKO host identity
   (`workspace_id`, host creation generation, and data root).
   `ChatResources`, foreground wrappers, and continuations copy that receipt
   without reacquiring focus. EKO places the root in the invocation value and
   wraps the receipt and identity in the framework's opaque, identified
   `InvocationResourceGuard`.
10. Each workspace Agent pool owns its own ToolManager and installs AutoIngest
    with the same immutable host identity, without retaining a control lease.
    AutoIngest requires exactly one invocation guard that both satisfies
    `retains::<ScopedWorkspaceIoReceipt>()` and matches that identity. Its root
    comes from the workspace-local descriptor, never `ToolContext.working_dir`,
    which writer isolation may replace with a worktree. EKO discards unrelated,
    mismatched, and ambiguous guards, clones only the exact receipt guard into
    its non-abortable blocking
    closure, and reports a typed persistence failure without writing when the
    root or typed receipt is absent. Local
    TaskRuntime main-Agent and Subagent paths propagate the same authority.
    Background recovery and cross-workspace targets that cannot produce the
    exact target receipt fail closed instead of borrowing another workspace's
    authority.
11. `create_complex_task` captures `WorkspaceIoInvocation` in its fully owned
    `RunPayload`. The canonical background driver carries it through planning,
    `task_execute`, main-Agent, and Subagent invocations; dropping the surface
    waiter therefore cannot strip authority from already-started persistence.
12. Read-only workspace IPC uses bounded `WorkspaceRegistry::inspect`.
    Switching enters the owned transition directly and performs its single
    activity update there; no stale pre-opened workspace record can overwrite
    a concurrent project-link revision.

## Consequences

- Focus changes do not retarget admitted operations.
- Deletion waits or fails busy while a scoped operation owns a host lease.
- Recreating the same workspace ID produces a different generation, so stale
  mutations fail closed.
- Relinking a project root increments the registry revision. Even if the old
  and new roots contain the same relative path and bytes, an old draft cannot
  pass generation validation against the new root.
- Blocking filesystem concurrency is bounded and Tokio heartbeat remains
  responsive.
- Dropping one async waiter does not detach accepted I/O: the service-owned task
  retains the semaphore permit, closure inputs and workspace receipt until
  stable settlement. Shutting down one AppState cannot close another live
  AppState, and a fully joined generation does not poison a later in-process
  restart.
- Analysis cleanup timeout or task failure is returned as a typed receipt and
  deliberately retains the workspace control owner. Analysis/workspace delete
  remains busy until a successful join proves the backend terminal safe point.
- `echo-agent` owns only the generic, opaque invocation-resource lifetime
  primitive. Root selection, receipt construction, persistence failure policy,
  and local product data remain EKO application concerns.
- Cancelling the async AutoIngest waiter cannot release its workspace lease
  while the started blocking closure is still running; delete and relink remain
  busy until that closure settles.

CLI, TUI, and channel `/analysis` and `/papers` commands continue to share the
same command implementations. Their synchronous domain calls use the same
adapter; no surface-specific CRUD implementation is introduced.
