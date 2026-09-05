# ADR 0039: Unified Agent Conversation Pane and Contextual Workspaces

- Status: Accepted
- Date: 2026-09-04
- Owners: `echo-agent::subagent`, `web-frontend`, `src/tauri`

## Context

EKO's right panel currently combines a permanent Tasks/Analysis/Research/Browser/Files/Automation
tab bar with a navigation-stack Subagent detail. Selecting a Subagent replaces the panel body while
the previous workbench tab remains active. The result presents two incompatible navigation
hierarchies and makes the Subagent look like content inside a workbench.

The Subagent detail reuses some chat rows but remains an inspector with separate controls and no
conversation composer. Framework Subagent events already carry thinking, final-token, tool, usage,
and terminal lifecycle data, while EKO's Tauri projection intentionally drops thinking and token
deltas. The payloads exist, but their public Subagent event transport has no common sequence,
timestamp, or complete identity on every event; its bounded broadcast can also report lag after
dropping events. The executor currently discards the existing inner `AgentEvent` envelope metadata
before producing `SubagentEvent`. The framework and application contracts therefore both need to
participate in the target behavior.

Industry references support keeping task state, execution output, and Agent contexts distinct:

- [Codex capability catalog](./0002-codex-tool-capability-catalog.md) records independent child
  contexts and typed message, follow-up, interrupt, list, and wait operations.
- [Claude Code capability catalog](./0003-claude-code-capability-catalog.md) separates task
  relationships from background execution handles.
- [EKO ADR 0038](./0038-unified-task-subagent-execution.md) fixes EKO at one Subagent level and
  defines one shared execution-admission policy across TaskRuntime and direct dispatch.
- [Claude Code Subagents](https://code.claude.com/docs/en/sub-agents) describes independent
  Subagent contexts, foreground/background execution, and nested Subagents. EKO adopts the
  independent-context model but explicitly does not adopt nesting.

Current OpenAI Docs pages returned HTTP 403 during this decision, so no claim about the current
Codex desktop layout is used as an authoritative premise.

The user-provided Codex desktop screenshot is accepted as a visual reference, not a runtime fact:
compact task navigation on the left, the primary conversation in the flexible center, an optional
contextual split on the right, flat surfaces, thin separators, restrained controls, and stable
bottom composers.

## Options

1. Keep the six tabs and restyle the Subagent inspector.
2. Remove both the tabs and the underlying workbench capabilities.
3. Remove the permanent tab bar, retain and relocate every capability, and render the selected
   Subagent through the same conversation presentation language as the primary Agent.

## Decision

Choose option 3.

- The center pane remains the primary Agent conversation. Selecting any Subagent opens its exact
  attempt in a resizable right split pane.
- Primary and Subagent panes share timeline and composer presentation primitives, while typed
  adapters preserve different delivery semantics: normal turn for primary, exact-attempt message
  for a running Subagent, and follow-up for a settled Subagent.
- A follow-up creates a fresh execution attempt for the same logical PlanTask. The pane shows an
  explicit attempt boundary and does not imply that private in-memory chat context survived.
- EKO permits only `primary Agent -> Subagent`. Child Agents do not receive `agent_tool` or
  `task_execute`, and runtime policy rejects delegation at depth one before creating a child run.
- The permanent Tasks, Analysis, Research, Browser, Files, and Automation tabs are removed. TaskRun
  control becomes a contextual run inspector; analysis, research, workflow, and extraction become
  dedicated workbenches; browser and files become contextual tool views.
- `Automation` is removed as a label because it currently groups workflows and structured
  extraction, not scheduler automation. Those capabilities retain their explicit names.
- The right pane has one discriminated contextual target. Subagent and workbench selections cannot
  remain independent states.
- The application shell follows a quiet three-column composition: collapsible task navigation,
  flexible primary conversation, and an optional resizable contextual pane. The center expands when
  context is closed; narrow viewports use overlays instead of compressing all three columns.
- Agent surfaces use flat neutral bands, thin separators, compact headers, ordinary reading-size
  typography, icon-first commands, progressive disclosure, and minimal elevation. They avoid
  decorative gradients, oversized radii, heavy shadows, nested cards, and permanent tool chrome.
- A contextual file or browser view may expose only its own compact local modes. It does not restore
  the removed global workbench tabs or create one tab per Subagent.
- The framework extends its existing versioned event-envelope semantics across the complete
  Subagent lifecycle. Every event has full invocation identity, monotonic per-attempt sequence,
  timestamp, stable event identity, parent correlation, and detectable gap semantics.
- Authoritative start/tool/usage/terminal boundaries may not be silently lost. Transient
  thinking/token deltas may be coalesced, but gaps are explicit and terminal full output reconciles
  final text.
- EKO enriches the framework envelope with workspace, PlanTask, revision, and attempt metadata. It
  does not generate substitute sequence values or add a Subagent chat store, execution runtime,
  mailbox, or lifecycle authority.
- Live messages retain the framework tracked-input receipt; EKO retains durable TaskRuntime receipt
  and follow-up guidance projection. They are joined in the view by exact attempt identity rather
  than merged into a new event state machine.
- Identical terminal error, summary, output, and unfinished-work text is displayed once.

## Trade-offs

- Extracting a genuinely shared timeline requires typed view adapters instead of mounting the
  primary `ChatPanel` inside the Subagent pane. This is more work than CSS changes but avoids
  inheriting primary-only branching, persistence, slash command, and queue behavior.
- Extending the public framework event envelope increases cross-repository scope, but generating
  order in Tauri would conceal upstream drops and create a second authority.
- Some live deltas are not currently durable. Reload must show the durable subset honestly until
  an authoritative journal retains additional display events; the frontend must not fabricate
  them from a terminal summary.
- Moving workbenches removes one-click permanent tabs, but slash commands, the command palette,
  contextual tool actions, and explicit run status restore discoverable entry points without
  competing with Agent navigation.
- Referencing Codex improves spatial clarity, but EKO does not copy its branding or infer its runtime
  semantics. EKO's TaskRun, attempt, receipt, file, browser, and tool contracts remain authoritative.

## Consequences

- `echo-agent` owns the reusable Subagent event-envelope, sequencing, gap, terminal-delivery, and
  depth primitives; its public docs and event example change with that API.
- `web-frontend` gains one right-pane navigation authority and a shared Agent conversation
  presentation layer without nested Subagent UI.
- `src/tauri` stops discarding displayable Subagent thinking/token events and losslessly enriches
  framework identity and ordering in GUI projection.
- Existing TaskRuntime, Subagent, tool execution, and Agent control authorities remain unchanged.
- Product feature documentation and any website image showing the old six-tab workspace must be
  reviewed during implementation.
- `SDK-Docs-Impact`: required; the public framework Subagent event contract and example change.
- `SDK-Skill-Impact`: none; Skill contracts do not change.
