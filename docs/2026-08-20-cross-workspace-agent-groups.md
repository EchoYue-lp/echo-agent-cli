# Cross-workspace Agent Messaging And Groups

Date: 2026-08-20

Status: M0-M8 complete

## Objective

Allow one EKO conversation to address another conversation by stable identity,
including when the target belongs to a different workspace and is not currently
loaded. Build persistent Agent groups on top of that transport while preserving
the existing `TaskRun -> PlanTask -> SubagentRun` execution authority.

The first usable boundary is durable cross-workspace delivery. Agent groups are
the next layer, not a prerequisite for the transport.

## Industry References

- Claude Code's official changelog documents named cross-session messaging,
  session discovery, waking stopped teammates, background resume, delivery
  failures, and the rule that an incoming Agent message does not carry user
  authority: <https://github.com/anthropics/claude-code/blob/main/CHANGELOG.md>.
- Codex app-server keeps a per-thread `cwd`, can load multiple threads, exposes
  spawned-thread ancestry and direct-input capability, and reports mailbox and
  active-turn diagnostics:
  <https://github.com/openai/codex/blob/main/codex-rs/app-server/README.md>.

The shared pattern is a stable global address, independently loaded session
runtimes, durable mailbox delivery, and explicit wake/resume. Neither system
implements cross-session work by changing one process-global workspace before
each message.

## Implementation Gate

### Framework/application placement

| Layer | Responsibility |
|---|---|
| Generic framework mechanism | Existing ReAct execution, `TurnSteerMailbox`, conversation/runtime-state traits, Task DAG validation, retries, cancellation, revision safe points, and Subagent execution |
| EKO product policy | Global workspace/session discovery, file-backed inboxes, workspace runtime lifetime, GUI/TUI/CLI/channel projection, Agent groups, unread state, and cross-workspace task targeting |
| Adapter boundary | Resolve `AgentAddress` to one workspace runtime, convert a typed message or task dispatch into an existing Agent/TaskRuntime invocation, and return typed receipts without owning another scheduler |

All new runtime and persistence code starts in `echo-agent-app-core`. No
`echo-agent` change is planned for M0-M8. A framework change requires separate
evidence that a missing primitive applies to other framework consumers.

### Whole-repository duplication audit

Searches covered both `echo-agent` and `echo-agent-cli` by proposed type names,
wire concepts, and adjacent behavior.

- No existing `AgentAddress`, `AgentMessage`, `AgentRouter`,
  `WorkspaceRuntimeHost`, `WorkspaceRuntimeRegistry`, or `AgentGroup` exists.
- `WorkspaceRegistry` is the existing workspace ID/root authority and will be
  reused for routing and discovery.
- `AgentPool` already gives conversations independent Agent instances, but its
  workspace stores and working directory are process-wide mutable overrides.
- `ForegroundTurnControl` is the foreground admission authority. Its identity
  must become workspace-qualified before two workspaces can contain the same
  conversation ID.
- Framework `TurnSteerMailbox` remains the only live-turn steer mailbox.
- TaskRuntime and framework Task tools remain the only DAG, revision, retry,
  cancellation, and SubagentRun authorities.
- Framework `TeamAgent` is a task-scoped in-memory team facade. It is not a
  persistent top-level conversation group and will not be repurposed.
- Surface-local ordinary-input queues carry attachments and same-conversation
  user follow-ups. They remain transport projections and never become Agent
  inbox authorities; cross-workspace Agent delivery uses only the app-core
  router.

## Current Constraint

`AppState::switch_workspace_inner` currently:

1. suspends foreground admission and TaskRuntime;
2. opens workspace conversation, checkpoint, memory, deletion, and task roots;
3. changes the process current directory;
4. mutates the primary Agent and every pooled Agent to the same stores/root;
5. rebinds one TaskRuntimeStore, plugin runtime, review integration, and watcher.

This is a global generation replacement. It cannot safely execute workspaces A
and B concurrently. Workspace selection must become a view/focus operation over
independent runtime hosts, not a process-wide execution boundary.

## Target Model

```rust
AgentAddress {
    workspace_id,
    conversation_id,
}

AgentMessage {
    message_id,
    from,
    to,
    payload,
    correlation_id,
    causation_id,
    origin,
    created_at,
}
```

`origin` distinguishes direct user input, Agent input, and system control.
Agent-originated content never satisfies a user HITL approval. Direct user
delivery is not gated by the automated Agent permission mode.

The router uses a file authority below EKO's global data directory:

```text
agent-router/
  groups.json
  inboxes/<workspace_id>/<conversation_id>/events.jsonl
```

Delivery is at-least-once. `message_id` is the idempotency key, and the target
must deduplicate before committing a transcript turn. The runtime does not
claim exactly-once execution across process crashes. The inbox never writes the
target transcript directly; the target Agent remains its sole transcript
writer.

Each loaded workspace is represented by an immutable-root
`WorkspaceRuntimeHost`. It owns the workspace-scoped AgentPool, conversation
store, runtime-state store, memory integration, TaskRuntimeStore, artifact
roots, and active-run lifetime. `WorkspaceRuntimeRegistry` loads hosts lazily
and cannot evict a host with an active turn, TaskRun, inbox delivery, HITL
request, or live runtime resource.

## Delivery Rules

1. Validate both addresses against `WorkspaceRegistry` and conversation
   storage.
2. Persist the message before attempting to wake or load the target.
3. If the exact target turn is steerable, use the existing steer path.
4. If the target is busy but not steerable, retain FIFO inbox order.
5. If the target is idle or unloaded, load its workspace host and conversation
   and invoke the shared `drive_chat` path.
6. Persist a queued, delivered, or failed receipt. The sender never waits
   synchronously for the recipient's answer.
7. Replies use the same router with correlation and causation IDs.

Loop depth, message size, retry count, and execution budgets protect against
framework bugs and accidental unbounded cost. They are not workspace permission
gates.

## Agent Groups

`AgentGroup` persists a group ID, display name, leader `AgentAddress`, and typed
members. It does not own an executor or a run state machine.

A group task creates one authoritative TaskRun in the leader workspace. An EKO
`TaskExecutionTarget` adapter may route a PlanTask to a member workspace. The
remote result is persisted back as the original PlanTask's SubagentRun attempt.
No `GroupRun`, remote mirror TaskRun, second DAG validator, or task CRUD surface
is introduced.

The committed PlanTask freezes `{group_id, subagent_role, address}`. At dispatch
time a thin EKO resolver validates the current group and acquires that exact
conversation-scoped Agent lease directly; it does not translate task execution
into an ordinary chat message or create another task mailbox. Group edits
therefore cannot silently retarget an already committed Plan revision.
Same-repository concurrent mutations continue to use the existing worktree
isolation policy in the selected member workspace.

## Milestones

| Milestone | Scope | Required real-path cutover |
|---|---|---|
| M0 | Contract, research, layer decision, duplication audit | This document and `MASTER-PLAN` become the governing record |
| M1 | `WorkspaceRuntimeResources` prepares all immutable workspace paths and file-backed stores | `switch_workspace_inner` uses the one resource factory and deletes its inline store/path construction |
| M2 | Workspace runtime host and registry; focused-host authority | Current workspace is represented by one registered host; repeated opens reuse its exact identity and reject root drift |
| M3 | Explicit execution scope and removal of process-global rebinding | Workspace switching changes focus only; a second host executes concurrently without `set_current_dir` or pool-wide store/root rebinding |
| M4 | Durable `AgentRouter`, address discovery, inbox replay, idempotent receipts | `agent_send` reaches an unloaded target and survives restart |
| M5 | Wake, steer, busy queue, cold resume, reply correlation | Target processing uses `drive_chat`; no transcript side-write or second mailbox |
| M6 | GUI/TUI/CLI/channel parity | Every surface projects app-core discovery/send/receipt state; none owns an Agent inbox |
| M7 | Persistent Agent groups and TaskRuntime target adapter | One leader TaskRun dispatches cross-workspace PlanTasks and receives canonical SubagentRun results |
| M8 | Delete transition-era paths and complete fault/soak gates | No duplicate executor/store/queue or execution-critical global cwd remains |

## Implementation Ledger

### M0-M2: complete

`WorkspaceRuntimeResources` is now the sole constructor for focused-workspace
paths, file ConversationStore, RuntimeStateStore, Memory Store, and aggregate
conversation deletion service. `switch_workspace_inner` consumes that resource
set and no longer duplicates those constructors. The existing publication
boundary remains intentionally unchanged until M2/M3: one primary Agent,
AgentPool, TaskRuntimeStore, ReviewIntegration, PluginRuntime, ConfigWatcher,
and process cwd are still rebound to the focused workspace.

Verification completed on 2026-08-20:

- focused resource tests: 3 passed;
- existing workspace transition tests: 4 passed;
- `cargo fmt --all -- --check`;
- workspace/all-target/all-feature Clippy with warnings denied;
- strict no-unwrap/no-expect/no-panic/no-unreachable Clippy;
- workspace/all-feature tests: app-core 1029 passed and 5 ignored, runtime-state
  integration 5 passed, CLI/TUI/Tauri 171 passed, JSONL subprocess 4 passed,
  doctests passed or were explicitly ignored;
- app-core no-default-feature check.

M2 adds one `WorkspaceRuntimeHost` per canonical workspace identity and one
process-level `WorkspaceRuntimeRegistry`. Repeated opens reuse the same host,
registry metadata can refresh without replacing immutable stores, and a
workspace ID cannot silently move to another root. `WorkspaceState.current` is
now a private focused-host reference instead of a separately mutable Workspace
copy. CLI linking and Tauri analysis/research callers use the `AppState` API;
there is no remaining external direct read or write of the focus lock.

Application commit: `f3b6f2c`.

The registry intentionally does not evict hosts yet. AgentPool, primary Agent,
TaskRuntimeStore, ReviewIntegration, PluginRuntime, ConfigWatcher, and process
cwd are still process-wide execution owners. M2 therefore establishes identity
and lifetime authority only; it does not advertise cross-workspace execution or
delivery. M3 must move execution-critical ownership/scope into the host path and
remove global rebinding before a second host can run.

M2 verification completed on 2026-08-20:

- runtime host/registry tests: 5 passed, including exact `Arc` reuse, metadata
  refresh, root-drift rejection, invalid roots, and conversation isolation;
- existing workspace transition tests: 4 passed, including focused-host
  metadata refresh across the transition boundary;
- `cargo fmt --all -- --check`;
- workspace/all-target/all-feature Clippy with warnings denied;
- strict no-unwrap/no-expect/no-panic/no-unreachable Clippy;
- workspace/all-feature tests: app-core 1031 passed and 5 ignored,
  runtime-state integration 5 passed, CLI 160 passed, main 11 passed, JSONL
  subprocess 4 passed, doctests passed or were explicitly ignored;
- app-core no-default-feature check;
- GUI-only check and tests: GUI library 105 passed, GUI entry 1 passed, JSONL
  subprocess 4 passed, doctests passed or were explicitly ignored.

### M3a: explicit turn scope and process-cwd removal

Every real GUI, TUI, CLI, JSONL, and channel chat entry now snapshots one
`WorkspaceExecutionScope { workspace_id, root }`. The exact value survives
foreground decoration and long-horizon continuation, is checked against the
active `TaskRuntimeStore` workspace before Agent execution, and is projected
to the framework's per-invocation working directory. An end-to-end tool probe
verifies that the selected root reaches `ToolContext.working_dir`; a separate
test proves a mismatched store/scope pair is rejected before the LLM runs.

`switch_workspace_inner` and `exit_workspace_inner` no longer call
`std::env::set_current_dir`. Direct GUI/TUI progress, file, Git, worktree, and
editor paths touched by this slice now read the application scope instead of
assuming a focus change mutated process state. The transition cancellation
test now observes an application publication boundary and proves process cwd
remains unchanged before, during, and after workspace transitions.

This checkpoint deliberately does not claim M3 complete. The primary Agent,
AgentPool, TaskRuntimeStore, ReviewIntegration, plugin projection, watcher
projection, and foreground keys are still process-wide or focus-rebound. M3b
must move those owners behind `WorkspaceRuntimeHost`, qualify execution keys
with workspace identity, and add the required two-host concurrent execution
test before cross-workspace delivery is enabled.

### M3b: host-owned execution and focus-only switching

Each loaded `WorkspaceRuntimeHost` now lazily owns an independent primary
Agent, AgentPool, TaskRuntimeStore, ReviewIntegration, conversation/checkpoint/
memory stores, artifact policy, and initial MCP/plugin runtime. Foreground keys
include workspace identity, so the same surface and conversation ID may run in
two workspaces without colliding. `switch_workspace_inner` and
`exit_workspace_inner` only publish focus and UI storage projections; they no
longer suspend foreground work, clear/rebind a process pool, or replace a live
task or memory generation.

GUI, TUI, CLI, JSONL, and channel turns atomically capture a
`ScopedChatRuntime`. Agent acquisition, TaskRuntime, memory review, transcript
metadata, steering, stop/reset, and cancellation remain attached to that exact
runtime even when focus changes. Workspace shutdown drains host TaskRun,
review, pool, plugin, and hook owners before the process-wide bootstrap owners.

The real-path tests run two hosts concurrently and prove distinct pools,
agents, task roots, ToolManagers, working directories, artifact roots, and
foreground keys. A focus-switch test keeps workspace A's turn and pool alive
while B is focused, then reuses A's exact host when focus returns.

This checkpoint still does not claim M3 complete. M3c must broadcast live model,
MCP, plugin, and config-watcher generations to every loaded host and add the
three-workspace MCP isolation/soak gate. Cross-workspace message delivery stays
disabled until that gate passes.

M3b verification completed on 2026-08-20:

- two-host concurrent execution, focus preservation, and workspace-qualified
  foreground tests passed;
- `cargo fmt --all -- --check`;
- workspace/all-target/all-feature Clippy with warnings denied;
- strict no-unwrap/no-expect/no-panic/no-unreachable Clippy;
- workspace/all-feature tests: app-core 1033 passed and 5 ignored,
  runtime-state integration 5 passed, CLI/TUI/Tauri 171 passed, JSONL
  subprocess 4 passed, doctests passed or were explicitly ignored;
- app-core no-default-feature check;
- GUI-only check and tests: GUI library 105 passed, GUI entry 1 passed, JSONL
  subprocess 4 passed, doctests passed or were explicitly ignored.

### M3c: loaded-host generation publication and activity proof

`WorkspaceRuntimeRegistry` now exposes stable snapshots of loaded execution
runtimes and per-host activity. Model mutation stages every process and
workspace pool before the single durable config write, then commits one
generation to existing primaries, cached conversation Agents, and future
Agents. MCP mutation persists one candidate, claims its namespace in every
host, refreshes every pool's future-Agent snapshot, and reconciles all
host-owned ToolManagers under the same generation cancellation token.

Plugin reads and mutations in GUI, TUI, and CLI resolve the focused host.
Install, uninstall, enable/disable, configure, and reload use that runtime as
the mutation authority and refresh every loaded follower after settlement.
Project/Local discovery remains host-scoped; User plugins converge across
hosts. The config watcher now accumulates `(workspace root, Agent)` targets
instead of rebinding one process-global target, so each host retains its own
project hooks while global config/hook changes reload all registered Agents.

M3 is now complete. The application retains exactly one model mutation owner,
one MCP durable config owner, and one plugin mutation owner per host; the
registry adapter only snapshots targets and fans out publication. No framework
API, second scheduler, SQLite dependency, permission gate, or process-global
cwd mutation was introduced.

M3c verification completed on 2026-08-20:

- three loaded hosts received the same active model generation in their
  primaries and future conversation Agents;
- 24 successive MCP generations converged across three independent
  ToolManagers while process cwd and host activity accounting remained
  isolated;
- one plugin generation reached the global runtime and three workspace
  runtimes;
- three registered watcher targets retained independent project hooks, and a
  malformed fourth target degraded without changing the other hosts;
- complete Rust, strict Clippy, no-default-feature, and GUI submission gates
  passed before the checkpoint commit.

### M4: durable address discovery and inbox acceptance

`AgentRouter` is an application-owned file service below
`agent-router/inboxes/`. `AgentAddress` reuses the existing workspace ID and
conversation ID authorities; discovery enumerates `WorkspaceRegistry` plus
each workspace's file `ConversationStore` instead of maintaining another
address index. Sending validates both source and target conversations before
acceptance.

Each inbox is an atomically replaced JSONL event stream protected by a stable
lock file. Target path segments are SHA-256 digests, so external conversation
IDs never become path components. `message_id` is the idempotency key: an
identical retry returns the original acceptance time, while the same ID with
different content fails closed. A corrupt inbox is reported and never silently
rewritten. The accepted message survives a new `AgentRouter` instance and is
replayed in FIFO order.

`send_agent_message_owned` opens only the target's immutable file-resource
host, verifies the persisted conversation, and writes the inbox before
returning `Queued`; it does not initialize target execution or side-write the
transcript. GUI IPC, CLI (`/agent-list`, `/agent-send`), and TUI expose this
same service. Channel projection and replacement of remaining surface-local
queues stay in M6.

M4 verification completed on 2026-08-21:

- accepted-message restart replay, exact duplicate retry, ID collision, and
  corrupt-inbox fail-closed tests passed;
- real AppState delivery to a validated conversation in an unloaded workspace
  returned `Queued` while every target activity snapshot remained execution
  unloaded;
- complete Rust, strict Clippy, no-default-feature, and GUI submission gates
  passed before the checkpoint commit.

### M5: wake, steer, cold delivery, and correlated reply

The application-owned delivery supervisor now claims one FIFO inbox head per
target address and persists every claim, defer, delivery, and terminal/retryable
failure in the same inbox event stream. A process restart reclaims an incomplete
claim with a new attempt identity; a stale attempt cannot settle it. Startup
recovery scans persisted endpoints after the workspace runtime pool exists and
reschedules every non-terminal inbox. Ordered shutdown first cancels and joins
delivery drivers, then shuts down foreground-turn ownership.

Delivery reuses the existing runtime authorities. A live, steerable target is
fed through the framework `TurnSteerMailbox` on its exact active turn. A busy
but non-steerable target is durably deferred, waits for the matching
`ForegroundTurnControl` settlement, and retries the same FIFO head. An idle or
unloaded target lazily opens its immutable workspace host, leases its existing
conversation Agent from the host `AgentPool`, and executes one ordinary
`drive_chat` turn under the new `Agent` foreground surface. The router never
writes transcript messages and owns no Agent executor or second steer mailbox.

Agent/runtime-authored input is marked runtime-authored and receives an empty
HITL dispatcher, so a message from another Agent cannot grant user approval.
Direct user delivery remains user-authored. A completed request queues a typed
reply through the same router with correlation and causation IDs; reply payloads
do not recursively generate another reply.

M5 focused verification completed on 2026-08-21:

- claim/defer/deliver/fail folding, FIFO ordering, restart reclaim, and stale
  settlement rejection passed;
- one real AppState route cold-started an unloaded target through `drive_chat`,
  persisted its assistant transcript, and delivered a correlated reply back to
  the source;
- an already-running target accepted the message through the exact live steer
  path without waiting for turn settlement;
- a busy non-steerable target preserved the queued head, then resumed it after
  exact foreground settlement with a second durable claim;
- foreground admission prevents an Agent delivery turn and any interactive
  surface from executing the same workspace-qualified conversation together.
- complete Rust workspace tests, both strict Clippy gates, no-default-feature
  checking, and the GUI check/test matrix passed after the real-path cutover.

M5 originally retained an explicit at-least-once window after transcript output
was persisted but before the router persisted `Delivered`. M8 closes that
completed-turn window with transcript-owned delivery markers and deterministic
turn/reply identities. Tool or external side effects that happen before a
completed transcript projection remain at-least-once across a process crash;
the runtime still does not claim impossible exactly-once execution across two
file authorities.

### M6: GUI, TUI, CLI, and channel parity

`AppState` now exposes the complete application-owned surface contract:
persisted endpoint discovery, optional resolution of the focused persisted
conversation as a reply address, durable send, and delivery-record projection.
The router remains the only inbox event owner and surfaces never read or fold
its files.

CLI, TUI, and channel expose the same `/agent-list`, `/agent-send`, and
`/agent-status` lifecycle through shared command projection functions. A
channel conversation that has not yet been persisted sends one-way; once it is
persisted, the same source-address resolution enables correlated replies. This
does not add a channel-specific queue or grant Agent input user authority.

GUI now has a visible Agent message dialog backed by the existing Tauri
discovery/send commands plus a new delivery-status query. It filters out the
current conversation, searches persisted targets, sends with the current
persisted source when available, and refreshes queued/claimed/delivered/failed
receipts from app-core. Its local state is only rendering state.

The pre-M6 audit found that GUI/TUI/CLI ordinary follow-up queues are not
duplicate AgentRouter ownership: they retain attachments, ordering, and
same-conversation user interaction. They remain in place. M6 removes no
ordinary-input behavior and establishes the narrower invariant that no surface
owns a durable cross-workspace Agent queue.

M6 verification completed on 2026-08-21:

- current-source resolution, delivery-record projection, cold target execution,
  correlated reply, and shared terminal receipt formatting passed;
- complete Rust workspace tests passed: app-core 1044 with 5 ignored,
  runtime-state integration 5, CLI 161, main 11, and JSONL subprocess 4;
- both strict Clippy gates, formatting, and app-core no-default-feature checking
  passed;
- GUI-only check and tests passed: GUI library 106, GUI entry 1, and JSONL
  subprocess 4;
- frontend Prettier, ESLint, 172 Vitest tests, and production build passed;
- real browser inspection at desktop size and 390x844 verified visible controls,
  stable two-pane/stacked layout, and no overlap or overflow.

### M7: persistent Agent groups and TaskRuntime target adapter

`AgentRouter` now owns one atomically replaced `groups.json`, protected by the
same stable cross-process lock discipline as inbox persistence. `AgentGroup`
contains one leader address and one or more uniquely addressed, dynamically
named Subagent roles. Group CRUD validates every persisted workspace and
conversation through the existing registries; it adds no SQLite store or
execution state.

`TaskExecutionTarget` is product metadata on the existing revisioned PlanTask.
The framework Task extension preserves it without learning workspace or group
concepts. `RealTaskDispatcher` resolves the frozen target to an existing
conversation-scoped Agent lease immediately before dispatch and, for writer
tasks, holds the same remote lease through worktree integration. The original
leader `TaskRuntimeStore` still claims, retries, cancels, reviews, and records
the one canonical `SubagentRun` attempt.

The resolver fails closed when the current TaskRun address is not the group
leader, the role is absent, or a group edit no longer matches the frozen member
address. Nested targeted tasks receive the same resolver without introducing a
second DAG loop. Framework `TeamAgent` remains a separate task-scoped in-memory
composition primitive and is not reused as persistent product state.

GUI exposes group list/create/update/delete beside Agent messages using
persisted conversation selectors. TUI, CLI, and channels share `/agent-group`
projection logic; scheduled TaskRuntime runs consume the same frozen target.
No surface reads or writes `groups.json` directly.

M7 real-path verification includes group restart persistence and validation,
remote host acquisition with leader/member drift rejection, framework metadata
round-trip, and a production dispatcher run proving that the remote Agent
executes while the leader TaskRun receives exactly one completed SubagentRun.
Submission gates passed on 2026-08-21: app-core 1053 tests with 5 ignored,
runtime-state 5, CLI/TUI/Tauri 162, main 11, JSONL 4, GUI-only 107+1+4,
frontend ESLint plus 173 Vitest tests and production build, both Clippy gates,
formatting, and app-core no-default-feature checking. Browser inspection at
1280x800 and 390x844 found no overlap or horizontal overflow.

### M8: transition cleanup and fault/soak gates

Cold delivery now derives its turn identity from the inbox `message_id`. Both
direct-user and Agent-authored delivery instructions carry the exact Message-ID
inside the target transcript while preserving their distinct authorship. After
restart, a reclaimed claim first reads the target ConversationStore: when the
exact delivery instruction and its completed assistant answer are already
present, EKO settles the Router receipt and recreates the correlated reply
without invoking the model or tools again.

Correlated replies use one stable identity derived from source address, target
address, and causation ID. Repeating reply enqueue after the transcript/receipt
crash window therefore returns the existing receipt instead of adding a second
reply. Message idempotency compares logical content rather than retry-local
timestamps, while conflicting payloads under the same ID still fail closed.

The transition audit found one canonical `AgentRouter`, one shared `drive_chat`
path, one TaskRuntime graph/executor/store, and no second Agent-group queue or
remote TaskRun mirror. The remaining `global_cwd` transition field was renamed
to `global_execution_root`: it is an immutable bootstrap root for the explicit
non-workspace execution scope, not mutable process cwd or a workspace rebinding
authority. Workspace focus changes continue to publish projections only.

M8 automated gates cover the completed-transcript crash window, second-claim
recovery, deterministic reply retry, stale settlement rejection, and three
independent workspace inboxes with 96 accepted messages. All three inboxes are
reopened concurrently after one abandoned claim per workspace; every message
settles once in its Router record, the three abandoned heads advance to attempt
2, and duplicate retries remain terminally idempotent. The existing real-path
M3/M5/M7 tests continue to cover three-host cwd/MCP isolation, unloaded target
resume, busy/live delivery, and one canonical leader TaskRun/SubagentRun.

Submission gates passed on 2026-08-21: formatting, workspace/all-target/
all-feature Clippy with warnings denied, strict no-unwrap/no-expect/no-panic/
no-unreachable Clippy, app-core 1055 tests with 5 ignored, runtime-state 5,
CLI/TUI/Tauri 162, main 11, JSONL 4, app-core no-default checking, and GUI-only
107+1+4. M8 changed no frontend source, and the M7 frontend lint/test/build and
desktop/mobile browser evidence remain the applicable UI gate.

## Acceptance Gates

- A message from workspace A reaches an unloaded conversation in workspace B,
  starts it once, and can receive a correlated reply.
- Restart at persist/claim/start crash points loses no accepted message and
  reclaims unfinished attempts. A completed transcript turn is deduplicated
  before model execution and its stable reply is recreated idempotently; side
  effects before transcript completion remain explicitly at-least-once.
- At least three workspaces can run simultaneously without crossing
  conversation, checkpoint, memory, task, artifact, MCP, or cwd boundaries.
- GUI, TUI, CLI, and channels use the same app-core discovery/send/receipt and
  Agent-group services.
- One group goal has one TaskRun graph and one SubagentRun identity per attempt.
- The application introduces no SQLite dependency, automated permission gate,
  process-global workspace execution authority, or legacy parallel-executor
  terminology.
