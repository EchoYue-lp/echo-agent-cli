# EKO Runtime Architecture

## Project Workspaces and actual locations

A Workspace is a project or engineering workspace; tasks and conversations belong to it, and TaskRun/PlanTask lists contain execution records. Workspace creation selects an existing local folder, creates one new local folder, or opens an SSH project. Creating a task does not create a folder. Code location is separate from local `Workspace.root` product data. Legacy manifests still resolve `project_root`, then `root`, without moving directories. The original manifest and `project_root_revision` own configured location and verified execution identity; see [ADR 0055](../adr/0055-project-workspaces.md).

The project tree, editor, changes/diff, Git/worktrees and Agent file/command tools use the same host target. GUI calls capture Workspace ID and generation; late responses cannot replace another location generation. The directory picker stays local. Remote paths are verified through the project connection, never local canonicalization. Metadata without a modification-time guarantee returns `null`. Saving retains the original lock/preimage semantics and the target's real comparison/replacement boundary. Unsupported operations are explicit; uncertain writes retain diagnostics and are not replayed.

The six non-native GUI file entries observe the original ProductDataIo operation for at most 30 seconds. Unconfirmed ends that wait while retaining physical work and its host receipt. Only real completion clears the same operation id, and late errors remain in its failure receipt. Local/global paths retain their previous waiting behavior. Retargeting and host close use the original idle gates; ordinary focus changes keep background work on its captured target. This observation does not replace the SSH backend's evidence of remote terminal or unknown outcomes.

The completed result envelope carries the same original operation until consumption or unobserved drop. A successful send into the result buffer does not acknowledge delivery: dropping that buffer preserves an unread error in the original failure receipt, while a consumed business error stays a normal caller result.

User rules, memory, conversations and journals remain local. Project AGENTS, Skills and Subagent sources use the selected project. Required read errors return at the original fallible model entry. After isolation establishes its actual cwd, a fresh Subagent prepares immutable instructions and project facts. The existing pre-model projection interface publishes those same read values after cold Execute/reset or restore and replaces old checkpoint projections by their original markers. The role root retains its original authority. This projection performs no file I/O and makes no failure admission decision.

Primary turns carry prepared project values in the existing turn registration, including the store-less, run-less embedding path when it supplies an explicit environment. The original EKO projector keeps Domain/TaskRuntime, turn policy, hot memory and command watches alongside those values. Concurrent primary verifications key this registration with their existing execution ID; the root message remains the original trigger. Fresh default and Plugin Subagents use one composition of the original Domain-only renderer and their own prepared checkout facts. Local product-data receipts remain identified guards, never command working directories.

Inline application configuration and user hooks/Skills retain local files and the original local executor. Project hooks/Skills capture project files and its executor; later host executor replacement cannot move registered origins. Hooks preserve inline, user-file and project-file order and replace the same UserConfig source as one unit. Project read errors block initial loading, while a failed explicit reload preserves the last valid set. Native configuration watching covers local configuration and local projects. SSH projects have no remote notify/poller; the existing explicit reload reads the same remote target.

A project terminal captures its project identity; explicit local and standalone SSH tabs keep their targets. It requires no Agent execution slot, LLM or full-auto mode. The original child/reader retains the host I/O receipt through physical settlement. Quit closes interactive PTYs before their dependencies and retains authentication masters until Agent/file users settle. Source and frontend tests do not replace real remote writes, native GUI or platform-specific acceptance.

This page records runtime ownership, lifecycle, workspace execution, and task
coordination. It is the current behavior reference; historical alternatives
remain in ADRs.

## GUI input acceptance and draft ownership

Queued additions acknowledge receipt from the original durable `queue_chat_input` result, without waiting for the model, physical tool or mailbox drain. Frontier refresh and tracked-steer observation stay with the original input service and attempt owner. Receipt, drain, context inclusion and terminal outcome are separate facts; receipt does not mean an active tool stopped or the correction executed. Before attachment preparation, one draft captures its external ID, Workspace/generation, Conversation/new-draft epoch and Domain choice. Retry retains that identity and encoded payload. The Hook's original turn/view fence and the GUI's draft, attachment and mount checks protect delayed callbacks: durable facts remain, while a newer draft or another task cannot be cleared. An explicit queued Domain choice cannot silently replace the current TaskRun binding.

Ordinary initial sending continues with the real conversation address returned by the original save owner. A delayed create cannot overwrite a newer selection or route input by rereading focus after saving. Direct Started acceptance and local slash-command success are distinct from a durable input receipt; the GUI clears only the still-owned original submission.

First-input projection ownership is captured before canonical conversation creation waits. The original save owner notifies the still-owned admission before selecting its canonical ID; only this proven null-to-ID transition may migrate that owner. Stop, unmount, navigation and newer drafts cannot revive an old projection. A successful save remains a fact even when list refresh or its view observer fails. A delayed `/clear` reset still settles its original conversation, while clearing the active projection requires the original Workspace, selection, load generation and new-draft epoch. Reset failure keeps the existing clear behavior only while that original view owns the action. These callbacks extend the original conversation lifecycle; they introduce no second store or execution owner.

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

Background Review is a separate app-core lifecycle. GUI, TUI, and CLI pass the
framework's lazy handle to a generation-bound owner, which durably admits the
operation before polling it, retains the generation through evidence settlement,
and cancels then awaits accepted work during shutdown. Startup reconciles
unfinished receipts before publishing the primary Agent. See
[ADR 0043](../adr/0043-background-review-owner.md).

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

The background `cancel_run` Tool returns `cancellation_requested` for the original
Store request-acceptance boolean. Its active-run path durably records interruption
and signals the original cancellation tokens while the Run may remain Running
and its execution claim stays owned. Observe the original TaskRuntime state and
physical-settlement receipts for final cancellation; the Tool reply does not
publish a `cancelled` completion mirror. Tool name, `run_id` input, Store API and
cancellation/settlement logic are unchanged.

## Failed publication and settlement evidence

Plugin cleanup keeps the original Agent, framework receipt, name registry and
other resource owners even when bootstrap fails. A subsequent bootstrap may
find debt by project root, but it retries against those original owners before
publishing its own generation. Unsettled debt rejects construction; cancelling
a shutdown waiter does not cancel the owned shutdown operation. See
[ADR 0022](../adr/0022-framework-prepared-plugin-generations.md).

MCP background operations enter through the same generation admission boundary
as configuration changes. An invalidated operation cannot start preparation
after a newer generation has confirmed its connection; started preparation
remains owned until its cleanup completes.

TaskRuntime uses the framework settlement result to decide whether attempt
evidence was accepted. `Superseded` rejects every request kind, appends no old
product evidence and emits no terminal event or continuation wake for that
attempt. Reading the current task status does not grant a stale claim the right
to publish that status. Healthy blocked/failed diagnostic evidence is retained.

## Canonical replay and review presentation

Task journal replay treats a present canonical `TaskExecution` as the complete execution record. Legacy flat status, claim, result and retry fields are fallback inputs only when that record is absent. A malformed record or mismatched task identity fails replay instead of authorizing fallback.

A Review candidate, its processing error, automatic Memory persistence and the application's Review Inbox receipt are independent facts. GUI and TUI retain the candidate and error together, distinguish persisted / not persisted / unknown, and only name an Inbox entry when the owner returned its actual evidence receipt. A local presentation never creates a new review outcome.

EKO's registered revision store exposes execution CAS through the same per-run file lock and atomic journal batch as direct runtime settlement. The framework computes and validates claims, interruptions and execution changes. The adapter compares the complete expected execution map and relation revision, rejects relation/context/spec drift, and never invents a plan revision for execution-only changes. Abandoned attempts use the final committed status and summary for both journal and live projection; an overridden request failure remains a separate exact-claim diagnostic.


## Product Convergence Disposition Inventory

Status: approved target, implementation pending. This inventory was checked against
`c262cfed8e77abed38a27c1fff93547997ec9486`, the local
`pre-retirement-2026-10-24` tag and `chore/Echoyue/full-capabilities-baseline`
branch. Both refs resolve to that commit. It records source reachability, not a
new native GUI, IM or cron acceptance result. [ADR 0050](../adr/0050-gui-product-convergence.md)
records the product decision. Paths below are repository-relative.

### Console entry points and retained owners

| Current source and reachable use | Future disposition and prerequisite |
| --- | --- |
| `Cargo.toml` defaults to `tui`; `src/main.rs` dispatches TUI/REPL/JSONL/channel paths, including a JSONL escape from a GUI build; `src/lib.rs` exposes CLI/TUI modules | Retire independent TUI/REPL/CLI/JSONL dispatch, console-only dependencies and help/configuration claims after retained paths are proven. A hidden flag or disabled default alone is insufficient. Keep a working desktop process entry. |
| `src/tauri/desktop.rs::DesktopArgs` is parsed by the existing `parse_desktop_args`, independently of console `Args` | Desktop accepts only model (including `-m`/`MODEL_NAME`), project, config and MCP overrides. Preserve Finder `-psn_` filtering and MCP argument → YAML → environment priority. Console resume/JSONL/permission/rendering flags are explicitly rejected by this entry. |
| `src/cli/modes.rs::run_channels_mode` constructs QQ/Feishu adapters, `ChannelManager`, session factories and shutdown/join; `src/cli/channels.rs::AppChannelMessageHandler` and `ChannelSessionCoordinator`, plus `channels/{input_pump,outbound,tool_render}.rs`, handle IM identity, replies and settlement | Retain IM behavior and move its application lifecycle into shared app-core ownership before deleting the launch mode. Preserve `(channel, conversation, sender)`, session incarnations, input-pump shutdown/join, outbound/tool rendering and channel HITL. GUI bootstrap does not currently start this manager; configuration fields are not evidence of GUI IM management. |
| `echo-agent-app-core/src/auto_memory/mod.rs::AUTO_MEMORY_ENABLED` is exported through the existing `api::auto_memory`; panels and remaining CLI/TUI callers all consume it | App-core owns the one process-global, initially true, non-persistent switch with the same Relaxed atomic reads/writes. GUI validates the workspace/generation before changing it. No per-workspace state or new store is introduced; later console deletion retains GUI memory controls and the original evidence lease. |
| `echo-agent-app-core/src/runtime.rs::ApplicationServices::compose`, `ApplicationLifecycleOwner`; `state/app_state.rs::start_scheduler_and_task_service`; `scheduler/{mod,runner}.rs` | Retain shared bootstrap, TaskRun boot recovery, SchedulerRunner, file-backed cron definitions and delivery ledger. `src/tauri/commands/scheduler.rs` and `web-frontend/src/components/scheduler/SchedulerPanel.tsx` remain adapters over them. No alternate GUI scheduler or automatic replay policy. |
| `echo-agent-app-core/src/terminal.rs::TerminalService`, `src/tauri/terminal.rs`, `web-frontend/src/components/terminal/` | Retain GUI PTY/xterm terminal create/write/resize/close and cleanup. Console product retirement does not retire an interactive terminal or Agent shell tool. The source now accepts explicit local/system-OpenSSH targets through this same owner; Plan06 native/remote qualification remains pending (ADR 0051). |
| `echo-agent-app-core/src/hitl/{dispatcher,channel_provider}.rs`; `src/tauri/commands/chat.rs::TauriHumanLoopHandler`; shared chat/pool/TaskRuntime owners | Retain generic HumanLoop request identity, GUI/IM response ownership, Task/Subagent and recovery. Audit `hitl/{tui_provider,repl_provider}.rs`, `src/cli/{jsonl,repl,editor,completion,keybindings,commands,command,extension_surface}.rs`, `cmd_impls/`, `src/tui/` and console render/config/tests/examples for console-only consumers before deletion; keep shared behavior through retained owners. |

Desktop bootstrap already composes shared services, including scheduler startup,
through app-core. Its event loop uses `run_return` followed by
`run_desktop_with_settlement`, preserving `ApplicationLifecycleOwner` settlement.
`src/tauri/mod.rs` currently toggles window visibility through a global shortcut;
this is not evidence that every close/tray/quit path has been accepted for the
new product. Target behavior: hidden windows leave IM/cron alive while the
process runs; actual exit stops admission, cancels and joins accepted work via
existing owners. No daemon continues after exit. Restart retains existing
conversation/session identity, occurrence idempotency and TaskRuntime recovery;
uncertain side effects are not silently replayed. GUI IM management, failure and
reconnect display, and hide/quit/restart acceptance must precede console removal.

### LSP consumer decoupling

The following table records the pre-retirement source inventory at the fixed
full-capability baseline. EKO source now removes these runtime consumers; it is
retained here to make the old-to-new disposition recoverable, not to advertise
current LSP support. Framework API deletion and SDK acceptance remain separately
owned. Rust/GUI validation of this consumer change is recorded with delivery.

| Consumer | Exact future decoupling scope |
| --- | --- |
| Features and tool registration | Remove the framework `lsp` requests in root and app-core `Cargo.toml` only in the implementation outcome. `runtime.rs::register_lsp_tools` is called at bootstrap and by `agent_pool/pool.rs` for workspace Agents; it registers diagnostics, definition, references, hover and status tools. Remove both call paths and manager ownership, not just GUI controls. |
| Plugin preparation/publication | `plugin_components.rs` consumes plugin LSP documents and validates LSP configuration. `plugin_runtime/{types,runtime,publication}.rs` owns `PluginLspRuntime`, config discovery, generation replacement, reload, shutdown, `LspServer` capability and language-count projections. Remove only LSP fields/branches and their consumers, while preserving prepared plugin receipts, rollback/debt owners, Skills, Hooks, MCP, Subagent, monitors, themes and output styles. |
| Configuration and shared command adapters | Remove `.lsp.yaml` discovery/watching and generation reload branches from `config_discovery.rs` / `config_watcher.rs`; remove LSP commands/kind/receipts in `extension_commands.rs`, LSP dispatch in `extension_control/service.rs` and developer help in `developer_commands.rs`. Audit `src/cli/cmd_impls/developer.rs`, `src/cli/extension_surface.rs`, `src/cli/channels.rs` and `src/tui/{commands,events}.rs`. Preserve the remaining extension dispatcher and watchers. |
| Tauri settings and tool calls | Remove `src/tauri/commands/lsp.rs::lsp_control`, its module and invoke registration in `commands/mod.rs` / `src/tauri/mod.rs`; retire `LspPanel`, the LSP settings navigation in `SettingsDialog.tsx`, `uiStore.ts`, `api/endpoints.ts` and related tests. `commands/config.rs` is a retained general configuration API, not a standalone LSP controller; audit affected projections without deleting it wholesale. |
| Generated and explanatory consumers | Narrow affected `web-frontend/src/generated/` extension/LSP types and barrel exports using the established generator only during implementation; reconcile docs, examples and feature/GUI/endpoint checks then. This document outcome changes no generated output, dependency lock, CI or example. |

Old `.lsp.yaml` configuration and plugin requests for retired A2A/LSP capability
must produce an explicit retired/unsupported-capability diagnostic. Removing
parsing or discovery must not silently ignore those inputs or report successful
component loading. Retain only the necessary input recognition and diagnostic
boundary, without a retired executor/controller. Acceptance must exercise both
old configuration and plugin requests and truthful loading of retained plugin
components. Preserve user conversations, tasks, memory, plugin files and
configuration files; none may be deleted as obsolete source code.

The source search found no direct EKO `a2a` feature request or `a2a`/`A2A`
production Rust use in root/app-core manifests and `src`/app-core source at this
baseline. That bounded negative result does not inventory the framework or SDK.
Their owners must retire their actual public APIs and exact accepted contracts;
ordinary AgentRouter messaging, Subagent, MCP and IM are retained.

### Ordering, preservation and evidence

1. Decouple EKO and SDK consumers while the old framework API still exists.
   EKO CI follows framework main; the SDK has its own exact framework revision,
   independent of superproject gitlinks. Only after supported consumers build
   without A2A/LSP may framework implementation and advertisements be removed.
   New framework APIs continue to ship before consumers adopt them.
2. Accept GUI-owned IM/cron startup, configuration, failure/reconnect, HITL,
   shared recovery, hidden-window operation and real-exit cleanup before console
   retirement. Keep ConversationStore facts, TaskRuntime `events.jsonl`, rebuildable
   projections and file persistence; no SQLite or second graph/store.
3. Keep the full-capability branch/tag as recovery anchors; restore through a new
   task branch and reassess against future main, not by retargeting preserved refs.
   Exa `4343f0b7` remains an independent local commit, with no push/main adoption
   implied. Framework/SDK/website inventories and cross-repository restoration
   evidence remain owned by their respective repositories/integrator.
4. This slice validates bilingual document parity, source references and diff
   scope. Actual runtime, native desktop, IM delivery, cron recovery and final
   consumer matrices belong to the later implementation outcomes. Historic ADR
   surface-parity and LSP implementation descriptions record prior behavior; they
   cannot override ADR 0050's forward product scope.

## Interactive local and SSH terminal sessions

`TerminalService` owns the original PTY, child, reader thread and retained settlement
handle. The desktop form accepts a host or existing SSH alias and optional account
and port; the system `ssh` receives literal argv and owns authentication, existing
configuration and host-key handling. EKO does not store credentials or infer login
from process start. Manual interaction is separate from Agent ShellTool permissions.

Each tab prepares xterm and Tauri listeners before create and stays mounted when
selected away or hidden. Raw bytes go to xterm, with 2,000 scrollback lines, a 1 MiB
output queue and 64 KiB input queue, both capped at 256 waiting chunks. Lag/overflow
is visible; there is no history replay, automatic reconnect or command resend.
Input overflow discards unsent queued input and stops further input on that view.

Close has a bounded observation wait and preserves the original native operations.
Only original child wait plus reader join permits session removal and one exit
receipt carrying the real code/signal. Kill/wait/read errors retain the same owner
and target; an irrecoverable read error cannot be erased by retry. App shutdown
closes admission before draining and retains the existing lifecycle owner on
failure. Direct-child settlement proves neither a process group nor remote side
effects stopped. See [ADR 0051](../adr/0051-ssh-terminal-sessions.md).

Qualification boundary: this change is source and Node evidence only. Rust tests,
new Tauri binaries, native window/theme acceptance and authenticated remote
interaction are not yet qualified; no platform or release support is claimed.


## Bound Subagent policy

EKO configuration supplies new-run defaults only. The original RunCreated execution profile owns a run’s captured depth limit; journal replay, checkpoint and resume identity preserve it. Primary is depth 0 and a real child is depth 1. The original TaskRuntime and framework dispatch own execution; no second task graph or policy store exists. See [ADR 0052](../adr/0052-gui-subagent-depth-policy.md). Source qualification does not replace native or exact-main validation.

### GUI IM/cron control and true desktop quit

The existing IM owner lives in app-core `channel_runtime`; bin entry points are thin forwards. ApplicationServices and Tauri share the same Arc. ConfigState publishes its UUID revision only after owned persistence; transport epoch and applied revision retain their distinct meanings. The lifecycle's original channel slot retains manager/coordinator/pump handles until real physical settlement, and blocks dependent close on unknown ownership. Desktop X hides; explicit Quit awaits the original receipt's final publication while keeping Tokio alive. Bounded observations elsewhere are unchanged. Completed failures can be final errors; unknown observations cannot grant finality. Cron views consume original edit/manual/history receipts and exact existing run facts. See [ADR0053](../adr/0053-im-cron-gui-control.md) for data flow, recovery boundaries, implementation references and unverified native/Rust acceptance.

## Task-bound domain plugins

Professional methods now enter the original plugin prepare/publication path as
EKO `domain.yaml` data, rather than four compiled profile templates. The complete
immutable binding is written in the original RunCreated event. TaskRun, graph
metadata and task extensions carry the same bounded identity reference; the
original journal/checkpoint fold reconstructs the full rules. Dynamic graph edits
retain the binding, tool ceiling and minimum artifact/check/semantic obligations.

Primary, planned Subagent, Verification and review paths use the stored rules.
The original admitted pool lease carries its published generation and the actual
Agent catalog handles, including cross-workspace targets. Restoring work checks
that identity; a current package cannot silently redefine an old task. General
chat retains its original no-plan completion. A lightweight domain uses the
original fixed Summary transaction and real event evidence, while formal domain
obligations require a real graph and the existing CompletionGate.

This is source implementation in progress. Adhoc `agent_tool` uses original
physical lease projections and same-registration admission; its existing model
projector reads the stored method even without a structured prompt payload. GUI
preparation reuses the original foreground token. These paths have new regression
definitions, but Rust type/tests, generated TypeScript, full integration and
native/main acceptance are pending. See [ADR0054](../adr/0054-task-bound-domain-plugins.md) and
[the package example](../../../examples/domain-plugin/README.md).

## GUI formal-task history reads

The task context pane discovers original formal TaskRuns in the selected Workspace. Its inventory and exact run selection extend the existing taskRuntimeStore; ordinary recent/new task conversations retain ConversationStore. `list_task_runs` binds product-data generation and status filters to finite live pages, with creation-time/run-ID order and explicit continuation. Read/repair failures preserve displayed content and report errors. The original latest-task provenance predicate also controls inventory visibility, so opening history never creates a plan or upgrades an ordinary turn.

Choosing an older run uses its exact run ID for details, polling and controls, rather than the conversation's latest run. Run/event pages do not create another task or event store. Event reads return a finite slice, a lossless next sequence and an unread-tail flag; partial Subagent history remains labelled incomplete. Original control receipts distinguish no-op, accepted request and scheduled/recovery retry from physical or semantic completion. Same-workspace generation changes and late requests are fenced by the original scope and GUI selection generation.

The bounded transport is not an index or a constant-time scan claim. Legal graph/query measurements and native light/dark, normal/minimum-window behavior remain required. Framework/SDK examples are unchanged; the coordinated website mirror waits for the qualified EKO source. See [the task panel guide](../guides/gui-task-runtime-panel.md) and [ADR 0008](../adr/0008-taskruntime-bounded-query-projections.md).

Historical inspection uses at most one additional presentation snapshot in the same Zustand store. It does not replace the current chat’s active run, Todo count, failure notice or message association. Task-panel actions carry their rendered run ID; conflict Continue uses the original admitted workspace/run and typed receipt, never a selected historical status. Late results cannot clear a successor prompt or update another presentation.
