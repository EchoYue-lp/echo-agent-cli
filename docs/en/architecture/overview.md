# EKO Architecture Overview

This page separates the current production architecture of `echo-agent-cli`
from the approved convergence target in [ADR 0050](../adr/0050-gui-product-convergence.md).
The framework owns ReAct, tools, memory, MCP and workflows. EKO has removed its LSP consumer paths; framework A2A/LSP APIs remain present
until both consumers pass acceptance;
framework public contracts remain owned by `echo-agent`.

## Product Boundary

EKO is a local personal assistant running on the user's machine. The approved
target has one GUI product entry, with IM messages and cron as retained background
triggers. GUI terminals, Agent shell tools, desktop launch arguments and the
independent SDK Host stdio protocol remain valid capabilities.

The current baseline still contains TUI, REPL, CLI/JSONL and channel adapters.
Their shared services remain authoritative during migration; the old requirement
to keep expanding all console surfaces has been superseded. Removing console
entry points must wait for GUI-owned IM lifecycle, cron and recovery acceptance.
This documentation change does not delete any runtime entry or protocol.

| Layer | Responsibility |
| --- | --- |
| `echo-agent` | ReAct, model protocols, tools, DAG, Subagent, memory/store traits, MCP/LSP, and workflows |
| `echo-agent-app-core` | EKO runtime, workspace and conversation identity, TaskRuntime file projections, AgentPool, HITL, Plugin, Browser, analysis and research policy |
| `src/cli` / `src/tui` | Current console adapters awaiting retirement; IM handlers and shared desktop dependencies must move first |
| `src/tauri` / `web-frontend` | typed Tauri IPC, GUI projection and interaction |
| `src-tauri` | desktop process entry, windows and platform facilities |

Workspace identity, GUI projections, worktrees, review policy, resource budgets,
and deletion policy remain application concerns. Reusable task graphs, state
transitions, model protocols, and tool contracts remain in the framework.

## App-Core Facade

`echo-agent-app-core::api` is the supported import boundary for CLI, TUI,
Tauri, channel, examples, and integration tests. It is a re-export facade; it
does not introduce a second runtime, store, DAG traversal, retry loop,
terminal reducer, or publication registry.

The physical modules keep authority-oriented boundaries. `state` owns
configuration, workspace/delivery scope, and `AppState`. TaskRuntime store and
executor modules own EKO file projections and adapters around framework
`RuntimeTaskService`/`RuntimeDagController`. Router, chat log, pool, extension,
plugin runtime, and infra modules follow the same inbox, journal, admission,
policy, publication, and factory boundaries.

AgentPool admission is backed by framework
`echo_agent::agent::admission::KeyedExecutionAdmission`. The framework owns
opaque-key leases, per-key process permits, retirement fences, close, and idle
waits. EKO retains Agent creation, capacity classes, cache eviction, workspace
generation, ToolControl, Plugin/MCP/model publication, and product receipt
mapping.

The split preserves JSON/JSONL, serde/TS bindings, file layout, error codes,
and current surface behavior; it does not require expanding retired product targets. A product-neutral framework primitive is admitted by
standalone semantics, dependency direction, and independent tests, examples,
and documentation; it does not wait for a second consumer. A separate EKO
contracts/domain/runtime crate remains a packaging decision that requires
dependency isolation, compile measurements, and multiple EKO consumers. See
[ADR 0025](../adr/0025-app-core-global-modularization.md) and the
[framework capability placement audit](../../../../docs/2026-08-30-framework-capability-placement-audit.md).

## Runtime Shape

Current implementation (console entry points remain until migration):

```text
GUI / TUI / CLI / JSONL / Channel
                  |
                  v
       echo-agent-app-core services
  AppState / AgentRuntime / drive_chat / TaskRuntime
                  |
                  v
       echo-agent framework primitives
 ReactAgent / tools / stores / DAG / Subagent / MCP
```

The application owns product identity, file facts, workspace policy, review
and worktree policy, and surface projections. The framework owns generic
execution, task graph, receipt, tool, store, and protocol primitives.

## Data Flows

All surfaces eventually use `drive_chat`/`drive_chat_turn`:

```text
input
  -> PreparedUserTurn
  -> ForegroundTurnControl admission
  -> workspace runtime snapshot
  -> AgentPool conversation Agent
  -> framework streaming execution
  -> ChatSink typed events
  -> transcript/checkpoint/tool projection
  -> TurnOutcome settlement
```

The framework `TurnReceipt` remains the single authority for terminal outcome,
provider usage, compaction count, final message identity, sequence, and exact
elapsed time across chat, continuation, and TaskRuntime lifecycle. EKO does not
define a `ChatTurnOutcome` wrapper; Task and webhook surfaces read only the
fields they need at their final product boundary.

External Skills follow the same ownership rule. EKO installs independent Skills
under `~/.eko/skills/`, discovers project Skills under the current project's
`.eko/skills/`, and consumes Skills from plugin generations; it does not
ship a built-in catalog, default active set, methodology baseline, or Tauri
Skill resources. Framework parsing and validation remain generic authorities,
while SkillsHub owns product installation and enablement. The version 3
`enabled-skills.json` file stores only `{enabled}` entries and falls back to an
empty set when unreadable. See [ADR 0041](../adr/0041-external-only-skills.md).

Task execution follows the single product model:

```text
TaskRun -> PlanTask -> SubagentRun
```

`PlanRevision` is an editable, versioned artifact. Framework `TaskStatus` is
the execution authority and `TodoItem` is a read-only query projection. EKO
adds file projections, workspace policy, review, worktree, and surface control
around framework task APIs.

See [runtime](./runtime.md), [persistence](./persistence.md),
[features](../features.md), and the relevant ADRs for detailed contracts.


## Convergence Boundary

The target flow is GUI / IM / cron into the existing app-core services and
framework Agent / Task / Subagent runtime. `ConversationStore` and the TaskRuntime
journal remain the respective conversation and task authorities; frontend state
is a projection. EKO uses files and memory, without SQLite. Framework storage
options for independent consumers remain a separate decision.

The GUI retains Side Conversation navigation and parallel panels. IM retains its
ordinary conversations, Subagent and existing fork behavior; it gains no dedicated
Side Conversation wire contract. Console-specific commands can leave with their
entry points while their shared semantics remain available through retained owners.

Hiding a GUI window leaves the process alive; actual application exit closes
admission and awaits lifecycle settlement. Restart uses existing IM identity,
cron occurrence and TaskRuntime recovery rules, without replaying uncertain side
effects or promising execution after process exit. GUI management and native
lifecycle acceptance remain future work. See the [source-backed disposition
inventory](./runtime.md#product-convergence-disposition-inventory).

A2A/LSP withdrawal requires EKO and SDK consumers to stop depending on them before
framework deletion. Additive framework APIs still ship before consumer adoption.
EKO CI follows framework main; the SDK Host has its own exact framework pin,
independent of the superproject gitlink. MCP, legal plugin components, local
terminal control and SDK Host stdio are retained. This is an explicit product
retirement, not permission to remove unused framework public APIs indiscriminately.


### GUI visual primitives

The GUI uses the existing `:root` / `.dark` semantic variables for surfaces,
readable secondary text, control contrast, borders and elevation. `uiStore` remains
the theme authority; plugin theme variables are inline overrides on the same root.
Cards, status badges, switches and brand icons consume these variables. Status
badges include a label and a distinct icon as well as color.

The Agent and context pane headers share a 52px rhythm and 32px chrome controls.
The left sidebar remains 264px, matching the existing context-pane geometry;
context targets, resize preferences and the mounted primary conversation remain
unchanged. Modal's backdrop blur and surface shadow are presentation only: portal,
nested stack, inert layers, focus restoration, Tab and Escape retain their existing
owner. Tauri mode uses the existing `VITE_EKO_TAURI` detection flag.

The sidebar and settings use readable labels and keyboard-visible actions. Settings
screens load on demand within the existing tab registry; loading or failure keeps
navigation, close controls and the primary workspace mounted. A failed screen can
return to the overview, while reloading the application retries its module load.

Browser fixture screenshots establish theme/layout coverage separately from native
Tauri window acceptance. This visual layer adds no chat, HITL, task, terminal or
plugin execution state, nor a new component framework or theme store.


### LSP consumer boundary

Primary bootstrap and scoped AgentPool construction no longer install the five
LSP tools or managers. `PluginRuntimeService` keeps an explicit project-root lock
independent of any language service; the root changes only at the original
publication/fail-closed boundary. MCP ownership tokens, lifecycle rollback,
monitor replacement and parked cleanup receipts keep their existing owners.

Only retired LSP component diagnostics are nonfatal to a mixed prepared generation;
other component failures retain the existing rejection policy. Reload receipts
report the retired component while legal Skills/MCP/Subagent/monitor/theme/style
components continue. The complete generic frozen diagnostics (including warnings)
retain severity and source identity in the existing receipt messages. EKO has no
LSP-specific document/field/method dependency. Late file changes cannot alter a
generation's receipts; cache invalidation uses the original framework integrator. Legacy config recognition is diagnostic only. Settings retain
lazy loading and local ErrorBoundary behavior; unknown old `lsp` keys use overview,
and old typed/invoke requests fail through existing unsupported/serde boundaries.

## Workspace and task navigation

Workspace, project and engineering project name the same existing Workspace. The sidebar separates New task, Recent tasks and Workspaces. New task opens a normal Conversation draft in the selected Workspace; it does not create a directory or stop another conversation running in the background. Recent tasks are a bounded, on-demand read projection of the existing workspace-scoped conversations, with at most two metadata queries at once; archived and Side Conversations retain their existing presentation paths. They are not the TaskRun execution graph.

The main composer places workspace selection and permissions at the lower left, and the real configured model selector at the lower right. Its workspace menu searches registered workspaces and opens the existing source form for an existing local directory, a new directory or an SSH device and remote directory. Remote entries display the verified execution location, never the local product-data folder. IM channels, scheduled tasks, terminal, settings, HITL and execution-task details remain reachable through their existing owners. The large primary composer is an explicit variant; Subagent inputs keep their compact layout.

You can add input while the conversation is running. The `已收到` (Received) notice means that input has been saved; the unchanged submitted text and attachments clear without waiting for the model or tools to finish. It does not mean execution or task completion. Attachment preparation or admission failure preserves the draft, and retrying an unchanged draft reuses the same submission. Repeated send events during admission do not create another submission. Late results do not clear a newer draft after navigation, reopening a conversation or editing. Stop remains a separate action.

Workspace/Conversation stores and backend lifecycle remain authoritative. Navigation uses transient UI intent and existing workspace generation checks to discard superseded selections; no second task store, recent-task database, permission model or SSH owner is introduced.

### Formal TaskRun history

“Task runs” opens the existing context pane's formal execution history for the current Workspace. This differs from the sidebar's ordinary recent/new task conversations. A history row selects that exact persisted run, including an older run in the same conversation. The current-conversation action explicitly returns to following that conversation. Status-filtered finite pages, visible read errors, original task details and typed control receipts share the same TaskRuntime facts. A next page or unread event tail is never presented as a complete inventory.

Selecting history leaves the current chat’s progress, failures and message association unchanged; controls act on the displayed run, while a pending chat Continue stays bound to its original run.

The default desktop page uses the existing native system-font fallback stack and makes no default remote font request. User-configured theme fonts remain user-owned.
