# ADR 0040: App-Core Subagent Event Projection

- Status: Accepted
- Date: 2026-09-05
- Owners: `echo-agent-app-core`, `src/tauri`, `web-frontend`

## Context

ADR 0039 requires EKO to render one Subagent attempt as an ordered Agent
timeline. The framework now publishes a versioned `SubagentEventEnvelope` with
stable identity, per-attempt sequence, parent correlation, bounded replay, gap
reporting, and retained terminal reconciliation.

The previous EKO path subscribed only to the bootstrap Agent's raw
`SubagentEventBus` in Tauri. It missed pooled and workspace Agents, reconstructed
Task identity from execution-id strings, allocated usage sequence locally,
cached ordinary-chat addresses in the desktop adapter, dropped thinking/token
events, and only logged broadcast lag. TUI maintained another raw subscription.
Those paths could not provide one durable or surface-neutral contract.

## Options

1. Keep per-surface raw subscriptions and add more Tauri/TUI caches.
2. Add a dedicated Subagent conversation store and event reducer.
3. Share one framework event bus across EKO Agent generations and project its
   envelopes once in app-core through the existing `ChatEventLog` and
   `ToolExecutionProjector`.

## Decision

Choose option 3.

- Bootstrap, pooled conversation, and workspace Agents receive clones of one
  `SubagentEventBus`. A single app-core service subscribes to its authoritative
  envelope stream.
- EKO's process Subagent admission is installed while an Agent is constructed
  or while task tools are registered. `execute_run` never mutates the Agent:
  `task_execute` can run under that same Agent's outer ReAct write lease, so a
  runtime write would self-deadlock before dispatch.
- `SubagentEnvelopeProjector` validates framework identity and content hashes,
  resolves TaskRun ownership without consulting current UI focus, and uses an
  exact foreground-turn fallback only for run-less ordinary dispatches.
- EKO never parses an execution-id string. Framework task, attempt, revision,
  lineage, stream, event, hash, sequence, timestamp, and parent identifiers are
  retained in generated `SubagentEventMetadata` on `ExecEvent`.
- The projector appends `ChatDriverEvent::Execution(ExecEvent)` to the existing
  `ChatEventLog` exactly once. Tool detail remains a derived projection in the
  existing `ToolExecutionRepository`; no new durable store is introduced. A
  failed tool projection is retried immediately and retained as bounded
  in-process debt for later events, while boot recovery can rebuild it from the
  authoritative ChatEventLog without appending the execution event again.
- A framework sequence jump first invokes `replay_after`. If bounded retention
  cannot provide a contiguous suffix, EKO commits a typed
  `subagent_stream_gap` event and still reconciles the retained terminal. Lag
  recovery scans the union of retained, active, and already-known streams. An
  active publisher keeps its immutable dispatch-start identity as an address
  anchor even when another stream evicts its whole replay suffix.
- `ChatEventLog` keeps only weak, ephemeral live-sink registrations for active
  turns. It invokes callbacks after releasing its registry lock. A committed
  background event without a live turn is exposed on the app-core committed
  projection stream for long-lived GUI, TUI, and REPL adapters. That stream has
  a bounded replay snapshot, and subscribers replay it after late attachment or
  broadcast lag with Chat event-id deduplication. One-shot JSONL has no
  unsolicited output owner after its response closes; request-scoped channels
  replay committed execution events from their durable conversation cursor on
  the next response.
- Tauri publishes committed chat envelopes and secondary tool summaries. It no
  longer subscribes to or interprets raw framework Subagent events. Active TUI,
  CLI, and channel turns receive the same `ChatDriverEvent::Execution` through
  their existing bound chat sinks; TUI and REPL also consume the committed
  fallback after a turn-local sink closes.
- The frontend imports the ts-rs-generated `ExecEvent` contract. Both live
  `chat://event` delivery and `replay_chat_events` call the same handler and
  Subagent reducer; framework event id/sequence make application idempotent.
- Delegated read/write PlanTasks no longer emit duplicate application start and
  terminal traces. Their TaskRuntime `SubagentAssigned`/`SubagentReleased`
  records remain the durable task lifecycle authority. Primary-direct task
  execution retains its app-owned trace because it has no framework Subagent
  envelope.
- Application shutdown keeps the projector alive until accepted TaskRuns,
  workspace pools, and the primary Agent have settled, then drains and joins it.

## Consequences

- Reload reconstructs the same typed execution events that were delivered live.
- Thinking, token, tool, usage, uplink, isolation, and terminal event classes
  are no longer silently discarded by the desktop adapter.
- A missing transient range is visible and does not invalidate retained durable
  boundaries or terminal output.
- Synthetic gap events keep ChatEventLog ordering separate from framework
  stream ordering, so a journal sequence can never suppress a later framework
  terminal.
- Surface adapters lose event identity and ordering policy; app-core owns the
  only EKO projection implementation.
- `SDK-Docs-Impact`: none. This decision consumes the framework API documented
  by framework ADR 0030 and changes only EKO product integration.
- `SDK-Skill-Impact`: none. Skill discovery and execution contracts are
  unchanged.
- `Website-Impact`: none. The website does not publish EKO runtime internals or
  this application surface, so no website content or generated index changes.
