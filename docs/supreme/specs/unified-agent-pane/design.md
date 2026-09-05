---
title: EKO unified Agent conversation pane and contextual workspaces
artifact: design
carrier: markdown
---

# EKO unified Agent conversation pane and contextual workspaces

## Problem and goals

The current right panel combines two unrelated navigation models:

- a permanent six-tab workbench switcher for Tasks, Analysis, Research, Browser, Files, and
  Automation; and
- a navigation-stack detail view for a selected Subagent.

When a Subagent is selected, its detail replaces the panel body while the previous workbench tab
remains visibly active. This makes the selected Subagent appear to be content inside the Tasks
workbench. The detail view also resembles a diagnostic inspector rather than the same Agent
conversation surface used by the primary Agent. Terminal failures may be repeated as raw error
text, a status, and `remaining_work`.

The target is one coherent multi-Agent interaction model:

1. The primary Agent and every Subagent use the same conversation presentation language.
2. Selecting a Subagent opens its live conversation in the right split pane.
3. The right pane has one active contextual target at a time and does not retain the permanent
   six-tab header.
4. Existing task, analysis, research, browser, file, workflow, and structured-extraction
   capabilities remain available without competing with Agent navigation.
5. Live intervention and follow-up reuse the existing authoritative Agent control contracts.
6. Reloaded and terminal Subagent runs remain inspectable without inventing a second conversation
   store or Agent runtime.
7. Framework and application responsibilities form one end-to-end contract: `echo-agent` owns
   reusable Subagent identity, ordering, delivery, and depth primitives; EKO owns product policy,
   durable projection, navigation, and rendering.

## Target behavior

### Shared Agent conversation surface

The center pane remains the primary Agent conversation. Clicking a Subagent row opens a resizable
right split pane whose header identifies the exact Subagent attempt and whose body uses the same
timeline primitives as the center pane:

- user or delegated input;
- thinking sections;
- tool executions in chronological ReAct order;
- assistant output and structured outcome details;
- artifacts and verification evidence when present.

The shared surface is a presentation component, not a new domain object. Primary conversation
messages continue to come from the existing chat store and conversation persistence. Subagent
content continues to come from framework lifecycle events, the TaskRuntime journal, tool execution
projection, and the existing Subagent outcome contract. A view adapter converts both sources to a
common ordered presentation model without copying execution authority.

The right-pane header contains only contextual Agent controls: close pane, Agent name, state,
parent/lineage, attempt identity, elapsed time, and compact usage where available. It does not show
the six workbench tabs and does not show a redundant Back action. Selecting another Subagent
replaces the selected target in the same pane.

"Same conversation surface" is a presentation contract, not a claim that a Task-oriented
Subagent is a persistent chat session. One Subagent attempt remains one isolated framework
execution. A follow-up creates the next attempt of the same logical PlanTask, and the shared pane
shows an explicit attempt boundary while retaining earlier attempts as read-only history. The new
attempt receives guidance through the existing typed contract; the UI must not imply that hidden
in-memory context survived across fresh Agent instances.

### Visual and spatial language

The user-provided Codex desktop screenshot is the visual and spatial reference: a quiet three-zone
work surface rather than a dashboard. EKO adopts the reference's clarity, density, and hierarchy,
not its brand, exact colors, or proprietary chrome.

On a desktop viewport the application reads as three continuous columns:

- a compact, collapsible left navigation for workspaces/tasks and their conversations;
- a flexible center conversation for the primary Agent; and
- an optional, resizable right contextual pane for the selected Subagent, browser, or file view.

The center expands when the right pane is closed. Opening the right pane must preserve a usable
center conversation width; resizing stops at explicit center/right minimums instead of allowing
either pane to collapse into clipped text. Center and right timelines scroll independently, while
their compact headers and bottom composers remain stable. On narrow viewports, left navigation and
right context become separate full-width overlays rather than compressed three-column strips.

The visual language is deliberately restrained:

- flat neutral surfaces separated by thin borders and spacing, with no floating section cards;
- one accent color for selection/progress, while success, warning, and failure retain semantic
  icon-plus-text treatment;
- compact headers, ordinary reading-size text, and regular line spacing rather than oversized
  titles or viewport-scaled typography;
- icon-first controls with accessible names and tooltips; persistent text labels only when the
  command would otherwise be ambiguous;
- small, low-contrast backgrounds for user messages and inline code, not large decorative bubbles;
- progressive disclosure for tool details, evidence, and metadata, keeping the default timeline
  focused on the conversation; and
- no decorative gradients, oversized radii, heavy shadows, nested cards, or permanently visible
  tool chrome.

The right pane may temporarily show a Subagent conversation, browser, file content, or diff. A
compact title row identifies that one target. Local modes intrinsic to the target, such as file
content versus diff, may use a small local segmented control; they must not recreate the removed
six-item global workbench tab bar or add one tab per Subagent.

### Input semantics

The shared composer looks and behaves consistently while retaining typed target semantics:

- Primary Agent: submit through the existing normal user-turn path.
- Running Subagent: submit through the existing exact-attempt `message` path.
- Settled Subagent: submit through the existing `followup` path, which targets the next attempt of
  the same logical Subagent task.
- A Subagent without a complete control identity: keep the timeline readable but disable the
  composer with a concise unavailable state.
- Interrupt remains an explicit icon action in the Subagent header; it targets only the selected
  exact attempt.

EKO allows only `primary Agent -> Subagent`. A Subagent never receives `agent_tool` or
`task_execute`, and an attempted dispatch at `delegate_depth >= 1` is rejected by runtime policy.
The UI therefore does not model or render nested Subagent navigation.

The UI must not infer delivery from optimistic local insertion. It renders the authoritative
receipt and lifecycle state already produced by the Agent control plane.

### Contextual workspaces

The permanent Tasks, Analysis, Research, Browser, Files, and Automation tabs are removed from the
right-pane header. Their capabilities are retained and relocated according to their purpose:

| Capability | Target entry and surface |
| --- | --- |
| TaskRun control | A compact run-status/plan action in the primary conversation header opens a contextual run inspector. |
| Analysis | A dedicated full workbench opened from slash command or command palette. |
| Research | A dedicated full workbench opened from slash command or command palette. |
| Browser | A contextual right-pane tool view opened by browser activity, browser output, slash command, or command palette. |
| Files | A contextual right-pane tool view opened by a file reference, tool artifact, slash command, or command palette. |
| Workflows | A dedicated workbench opened from slash command or command palette. |
| Structured extraction | A dedicated workbench opened from slash command or command palette. |

`Automation` is not retained as a product label because the current surface contains workflows and
structured extraction rather than the scheduler. Those two capabilities keep their explicit names.
Existing slash commands remain stable entry points; only their destination presentation changes.

The right pane therefore owns one discriminated contextual target, conceptually one of:

- selected Subagent conversation;
- TaskRun inspector;
- browser session;
- file document or diff; or
- closed.

Dedicated analysis, research, workflow, and extraction workbenches are primary application views,
not right-pane siblings of a selected Agent. Changing workspace or conversation clears stale
contextual targets that do not belong to the new scope.

### Failure presentation

A terminal failure is presented once as the terminal state and primary error. Structured
`remaining_work` is shown only for distinct actionable work not equal to the primary error or
summary. The UI must not display the same text as raw output, error, summary, and unfinished work.
Transport or identity failures remain distinguishable from a Subagent's own task result.

## Scope and non-goals

### In scope

- GUI information architecture for the center conversation and right split pane.
- Shared primary/Subagent timeline and composer presentation.
- Identity-preserving projection of Subagent thinking, token, tool, usage, and terminal events,
  with explicit gap detection for transient streaming deltas.
- Reload reconstruction from existing authoritative journals and projections.
- Contextual relocation of the six current workbench entries.
- Responsive desktop and mobile behavior, keyboard focus, accessibility labels, and loading,
  empty, failure, cancellation, and stale-identity states.
- Codex-inspired visual cleanup of the shared application shell and Agent panes: restrained
  surfaces, compact chrome, stable composers, and removal of unnecessary card decoration.
- Documentation and regression coverage for the new navigation and event contracts.

### Non-goals

- Removing analysis, research, browser, file, workflow, or extraction capabilities.
- Creating a new Agent, Task, conversation, mailbox, or persistence authority.
- Changing the semantic distinction between `message` and `followup`.
- Changing Subagent concurrency, iteration, or timeout policy. The already-decided EKO depth-one
  policy is part of this design because it determines the visible Agent hierarchy.
- Redesigning TUI rendering in this delivery. The underlying event and control contracts must
  remain surface-neutral so TUI/CLI/channel parity is not reduced.
- Treating hidden chain-of-thought as required display content. Only framework events already
  classified as displayable thinking are projected.

## System boundaries

### Framework

`echo-agent` remains the authority for Subagent execution, attempt identity, depth enforcement, and
lifecycle events. It already emits `DispatchThinkingStarted`, `DispatchThinkingDelta`,
`DispatchThinkingEnded`, `DispatchTokenDelta`, tool events, usage, and terminal outcomes, but the
current raw `SubagentEvent` stream is not sufficient for an ordered conversation projection:

- only `DispatchStarted` carries conversation/message addressing;
- events have no common sequence, timestamp, stable event id, or parent-event link;
- the executor wraps inner `AgentEvent`s in the existing `EventEnvelope` and then discards that
  metadata before emitting `SubagentEvent`; and
- the default broadcast buffer is bounded and a lagging consumer can miss arbitrary events.

The framework must extend its existing versioned envelope mechanism to cover the complete
Subagent lifecycle. Every emitted Subagent event carries full invocation identity, a monotonic
per-attempt sequence, timestamp, stable event identity, and parent correlation. The implementation
must reuse the existing `EventIdentity`/`EventEnvelope` validation and sequencing semantics rather
than create an EKO-specific sequence in Tauri.

Delivery distinguishes authoritative execution boundaries from transient deltas. Dispatch start,
tool start/terminal, usage, and Subagent terminal events may not be silently lost. Thinking/token
deltas may be coalesced for bounded transport, but any dropped range is detectable from sequence
gaps and terminal full output reconciles final text. The public framework contract must document
lag, resubscription, and terminal reconciliation behavior for all consumers.

User-to-Subagent control is not folded into that execution event stream. Live `message` continues
to use the framework's tracked input receipt, while EKO persists product-level receipt transitions
and `followup` guidance in the existing TaskRuntime journal. The shared UI joins these two typed
projections by exact attempt identity without creating another lifecycle.

The framework retains generic nesting support for independent consumers through
`NestedDelegationPolicy`; EKO supplies maximum depth one and removes delegation tools from child
capabilities. This keeps product policy out of the reusable framework while enforcing it at runtime.
The product policy and shared execution-admission decision are defined by EKO ADR 0038; this design
consumes that decision and does not introduce another concurrency or hierarchy authority.

### EKO application core and Tauri adapter

EKO owns product addressing, journal replay, GUI projection, workspace scoping, the depth-one
capability policy, and the decision to show an Agent conversation in a split pane. The Tauri adapter
must project displayable thinking and token events instead of dropping them. It enriches the
framework envelope with EKO-only workspace, PlanTask, plan revision, and attempt metadata without
reconstructing framework identity from string formats or an in-memory `DispatchStarted` map.

Streaming deltas may be coalesced for transport and rendering efficiency, but coalescing must not
change text, cross lifecycle boundaries, merge different attempts, or become a new durable source
of truth. On an explicit sequence gap, EKO rehydrates authoritative lifecycle/tool/terminal state
and marks unavailable transient content rather than continuing as if the stream were complete.
Durable TaskRuntime replay remains the recovery source for Task-bound Subagent runs; ordinary
conversation-bound runs use their existing conversation/run projection. If current durability
cannot reconstruct displayable thinking or partial final text after restart, the UI shows the
durable tool and terminal history honestly rather than fabricating missing deltas.

### Web frontend

The frontend owns the view-only selection and layout state. The existing Subagent run and tool
execution stores remain projections. A shared ordered view model composes primary or Subagent
content for rendering; it does not duplicate chat or runtime state machines.

The right-pane state has a single target authority. Workbench navigation and Subagent selection may
not remain as independent states whose visual selections can disagree.

## Core structure and data flow

### Live Subagent flow

```text
framework versioned Subagent event envelope
  -> EKO address enrichment and durable boundary projection
  -> Subagent/tool projection stores
  -> ordered Agent conversation view adapter
  -> shared timeline in the right split pane
```

Tool rows continue to use the existing typed tool execution owner key. Thinking and final-token
deltas append only to the matching exact execution and sequence. Because EKO forbids child
delegation, a Subagent timeline never contains another Subagent row. Framework consumers that allow
nesting still retain parent-event correlation in the generic envelope.

### Reload flow

```text
workspace + conversation selection
  -> load conversation and TaskRun projections
  -> replay Subagent lifecycle and tool ownership
  -> resolve selected exact attempt if still present
  -> render recovered timeline or close stale selection
```

A selected run that no longer resolves after workspace/conversation change is closed. A settled run
that remains in authoritative history remains selectable. The UI must never attach a terminal event
from an older attempt to a newer follow-up attempt.

### Contextual tool flow

```text
slash command / command palette / tool artifact / explicit header action
  -> one contextual destination
  -> dedicated workbench or right-pane target
  -> close returns to the primary conversation or selected Subagent context
```

Opening a browser or file view may temporarily replace the right-pane Subagent target. Closing that
tool view returns to the previously selected Agent only when the identity still belongs to the
current workspace and conversation; otherwise the pane closes. This is a view stack only and does
not affect Agent execution.

## Edge and failure scenarios

- **Subagent completes while selected:** keep the pane open, settle streaming indicators, switch the
  composer to follow-up semantics, and retain exact attempt history.
- **Subagent is interrupted:** show one cancelled terminal state and retain completed tool history.
- **Message races with settlement:** display the typed rejected/queued receipt; do not relabel it as
  delivered. Offer follow-up only after authoritative state confirms settlement.
- **Follow-up creates a new attempt:** preserve the prior attempt and select the newly admitted
  attempt when its authoritative lifecycle event arrives. Render an attempt boundary and do not
  imply that the fresh Agent instance retained private in-memory chat state.
- **Subagent attempts to delegate:** reject before dispatch with the typed depth/capability error;
  no hidden child run or UI row is created.
- **Event receiver lags:** detect the sequence gap, rehydrate durable boundaries, reconcile final
  output from the terminal event, and mark unrecoverable transient thinking/token ranges as absent.
- **Missing streaming history after reload:** show available durable events and a neutral indication
  that earlier live text was not retained. Do not synthesize it from the terminal summary.
- **Duplicate terminal fields:** deduplicate identical normalized error, summary, output, and
  `remaining_work` text while preserving distinct structured evidence.
- **Large output:** reuse the existing bounded tool-output/artifact presentation; the shared pane may
  not eagerly mount an unbounded DOM copy of full artifacts.
- **Mobile viewport:** the selected Agent or contextual tool becomes a full-width overlay with the
  same close action. It must not compress center and right conversations side by side.
- **Narrow desktop viewport:** preserve explicit minimum widths, wrap or hide secondary metadata,
  and never overlap the primary conversation, Subagent pane, or either composer.
- **No active TaskRun:** the primary conversation remains fully usable and no empty Tasks tab is
  shown.
- **Workspace switch:** discard stale local selection and reload only identities belonging to the
  selected workspace/conversation.

## Key decisions and trade-offs

### Remove permanent workbench tabs, retain capabilities

Deleting the capabilities would lose useful product functions. Keeping six permanent tabs makes
unrelated workbenches appear equal to the selected Agent and leaves two conflicting navigation
systems. Contextual and dedicated entry points preserve capability while making the active Agent
the primary interaction object.

### One presentation language, distinct typed commands

Primary and Subagent panes should look and behave consistently, but their commands are not
interchangeable. A shared component with typed adapters avoids both UI drift and a false unified
backend endpoint.

### Reuse lifecycle projections instead of a Subagent chat store

Adding a Subagent conversation store would duplicate framework execution, TaskRuntime journal, and
Agent control authority. The selected pane is reconstructed from existing lifecycle and tool
projections. This keeps one execution truth while accepting that some historical live-only deltas
may be unavailable until explicitly retained by the authoritative journal.

### Framework envelope, EKO projection

Assigning sequence numbers only after events reach Tauri would make EKO report a complete order even
when the framework broadcast already dropped data. The framework therefore owns sequence and gap
semantics. EKO persists and renders product-addressed projections without turning the framework into
a GUI-aware component.

### Context view stack, not another product state machine

Remembering the previously selected Agent while a file or browser view is temporarily open is local
navigation state. It must not be persisted as Task or Agent lifecycle state.

### Codex-inspired structure, EKO-owned interaction

The Codex reference demonstrates that a task navigator, primary conversation, and contextual split
can coexist without dashboard chrome. EKO adopts that spatial hierarchy and restrained density, but
keeps its own TaskRun, Subagent attempt, tool, file, browser, and receipt semantics. Copying visual
chrome without these EKO contracts would reproduce the current mismatch in a cleaner skin.

## Industry references

- The repository's Codex capability catalog records Subagents as children of the current task with
  independent context and typed `send_message`, `followup_task`, `interrupt_agent`, list, and wait
  operations. EKO keeps those control meanings distinct while presenting the selected Subagent as a
  conversation.
- The same Codex catalog records versioned item lifecycle events. EKO follows that observable-event
  pattern by requiring framework-owned Subagent event identity and ordering rather than generating
  UI-only order after transport.
- The repository's Claude Code capability catalog separates task relationships from background
  execution handles and keeps task state and process output from becoming competing stores. EKO
  likewise keeps Agent conversation presentation separate from TaskRun control and tool
  workbenches.
- Claude Code's official Subagent documentation describes Subagents as independent contexts that
  return results and supports foreground/background execution and nested Subagents. EKO adopts only
  the independent-context model and deliberately rejects nested Subagents under its product policy.
  The selected one-level context is exposed in a split pane suited to EKO's GUI.
- Current OpenAI Docs pages could not be fetched in the investigation environment (HTTP 403), so no
  unverified claim about the current Codex desktop layout is used as a design premise.
- The user-provided Codex desktop screenshot is used as a visual reference for the three-column
  shell, compact navigation, flat conversation surface, contextual right split, and bottom composer.
  It is a visual input, not evidence for undocumented Codex runtime behavior.
- EKO ADR 0038 fixes the product hierarchy at one Subagent level and defines one shared execution
  admission across TaskRuntime and direct dispatch. This UI design must consume, not duplicate,
  those runtime decisions.

## Reuse and implementation constraints

- Reuse `MessageBubble` timeline primitives, `ThinkingSegment`, `ExecutionProcessGroup`,
  `InlineToolCall`, `SubagentStreamBlock`, `SubagentOutcomeView`, `ChatInput` styling, the current
  resizable layout, and existing Zustand projections where their contracts fit.
- Do not reuse `ChatPanel` wholesale for Subagents because it owns primary conversation branching,
  slash commands, queued user inputs, persistence, and normal-turn delivery. Extract presentation
  primitives and inject typed behavior.
- Reuse existing framework `SubagentEvent` payload variants, `EventIdentity`/`EventEnvelope`
  mechanics, `NestedDelegationPolicy`, and Agent control receipts. Extend the framework envelope
  boundary; do not add an EKO-only event sequence, second mailbox, reducer, execution supervisor, or
  persistence store.
- Reuse Tauri and React facilities already installed. No new UI, routing, event, or state-management
  dependency is required.
- Preserve TUI/GUI/CLI/channel functional parity at the control-contract level. GUI-only layout
  decisions may not remove control functions from another surface.
- Preserve UTF-8-safe truncation and existing bounded artifact/tool-output behavior.
- Because the framework event contract changes, update its public Rust documentation and the
  Subagent event example, and compile/test that example with the public API.
- Land and verify the reusable `echo-agent` event contract before EKO consumes it; the application
  may not temporarily invent a parallel Tauri-only ordering contract.
- The EKO adapter must consume only the framework envelope and add product metadata losslessly; its
  generated TypeScript contract requires exact serialization and round-trip tests.

## Acceptance criteria

1. No permanent Tasks, Analysis, Research, Browser, Files, or Automation tab bar appears above a
   selected Subagent or in the default right pane.
2. Clicking any visible Subagent opens the exact run/attempt in a resizable right split pane;
   clicking another Subagent switches the pane without adding tabs.
3. Primary and Subagent conversations use the same visual timeline primitives for thinking, tools,
   final text, attempt boundaries, and structured results.
4. A running Subagent has a composer that sends an exact-attempt message; a settled Subagent has a
   composer that queues a follow-up; receipt failures are visible and are not shown as success.
5. Every framework Subagent lifecycle event carries complete identity, stable sequence/timestamp,
   and detectable gap semantics; EKO does not synthesize these facts after receipt.
6. Tool events remain in original chronological order relative to displayable thinking and final
   output for both live execution and supported replay; authoritative boundaries survive delta
   bursts without silent loss.
7. Reload preserves every durable Subagent tool and terminal event, restores a resolvable selected
   attempt, and closes stale selections without attaching data to another attempt.
8. A terminal failure is shown once; identical error/summary/remaining-work text is not duplicated.
9. TaskRun controls remain reachable from the primary conversation; analysis, research, workflow,
   and extraction remain reachable as dedicated workbenches; browser and files remain reachable as
   contextual tool views; existing slash-command capabilities are preserved.
10. Desktop resizing and mobile full-width overlay behavior contain all headers, controls, timeline
    text, and composer content without overlap or horizontal clipping.
11. Keyboard focus enters the opened pane predictably, close and interrupt controls have accessible
    names, and status is not communicated by color alone.
12. EKO registers no delegation/task-execution tools on Subagents, and framework runtime rejects an
    EKO dispatch at `delegate_depth >= 1` without creating a child run.
13. Frontend tests cover navigation authority, live/settled composer routing, attempt boundaries,
    failure deduplication, workspace switching, and responsive render states. Framework tests cover
    envelope identity/order, lag/gap behavior, terminal preservation, and depth rejection; EKO
    Rust/Tauri tests cover lossless metadata enrichment, tracked receipt integration, and projection
    of every event class.
14. At representative wide desktop, compact desktop, and mobile viewports, screenshot verification
    shows a compact left task navigator, readable primary conversation, optional contextual pane,
    independently scrollable timelines, and stable bottom composers with no overlap or clipped text.
15. The default Agent surfaces use flat bands and spacing rather than nested/floating cards; no
    decorative gradient, oversized radius, or heavy shadow competes with conversation content.

## Impact assessment

- `SDK-Docs-Impact`: required. `echo-agent`'s public Subagent event transport contract and example
  must document versioned identity, ordering, gap, and terminal reconciliation semantics. EKO
  product architecture and user-facing feature docs also require synchronization.
- `SDK-Skill-Impact`: none. No Skill discovery, activation, instruction, or execution contract is
  changed.
- Examples: update and compile the framework Subagent event/communication example against the
  envelope API.
- Website: review is required during implementation; update only if the website depicts or explains
  the removed six-tab right workspace or the old Subagent inspector.
