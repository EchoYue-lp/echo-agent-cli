# ADR 0013: Subagent Control Receipt Projection

- Status: Accepted
- Date: 2026-08-27

## Context

TaskRun Subagent control previously called a removed framework `send_message`
operation and treated its return as an application `Delivered` terminal. That
collapsed mailbox acceptance, model-context drain, and owning-turn settlement.
It also left GUI, TUI, CLI, and channel steer adapters on the legacy untracked
`steer_input` path.

## Decision

`echo-agent` owns the live lifecycle through
`steer_input_tracked -> SubagentMessageReceipt -> AgentSteerReceipt`. The EKO
application journal keeps `SubagentGuidanceQueued` as its persisted command and
uses only typed `SubagentGuidanceMailboxAccepted`, `SubagentGuidanceDrained`,
and `SubagentGuidanceSettled` facts after that persisted boundary. The obsolete
mailbox-delivery compatibility event has been deleted from parsing, reduction,
fixtures, and production; this project carries no compatibility substitute.

`SubagentControlReceipt.status = accepted` is derived from the framework
mailbox phase; `phase` carries the durable application
boundary and `outcome` is present only at owning-turn settlement. The
application never infers drain from rendered text, stream EOF, or foreground
status. GUI/TUI/CLI/channel active steer calls use the tracked framework API and
retain their existing FIFO fallback on rejection or terminal-before-drain.

Before invoking the framework live-message effect, EKO reserves an operation
supervisor receipt for the observer that will retain the framework receipt
through durable drain and turn settlement. Reservation failure appends a typed
rejection and performs no framework effect. Once accepted, caller drop and
shutdown cannot orphan the observer. Next-attempt guidance uses the same
Persisted -> MailboxAccepted projection and reaches TurnSettled in the existing
atomic `SubagentReleased` journal batch.
Because the framework future-attempt queue has no tracked context-drain receipt,
that path settles with `drained=false`; Subagent completion is not relabelled as
proof that the queued text entered model context.

The existing `SubagentReleased` writer terminalizes every non-terminal guidance
and interrupt command bound to that exact physical attempt in the same journal
batch. This is the fallback when an async receipt observer loses a persistence
race with Subagent release. A later observer sees the already-terminal receipt
as an idempotent duplicate; it does not append a conflicting drain or terminal.
Lifecycle writes and interrupt settlement use bounded exponential retry. An
exhausted write releases the operation receipt with explicit supervisor debt so
shutdown is bounded and boot reconciliation owns the remaining durable marker.

Boot reconciliation scans exact command identities after ordinary TaskRun
recovery. A live command left at Persisted, MailboxAccepted, or Drained is
terminalized as Dropped unless an exact `SubagentReleased` fact supplies a more
specific outcome. A future-attempt command remains Persisted until its intended
attempt starts; once handed to the framework queue it is terminalized rather
than resent. A durable Drained fact remains `drained=true` during recovery.
An orphan `SubagentInterruptRequested` is terminalized once as an unaccepted
interrupt rather than remaining pending or replaying the interrupt effect.

Cold Conversation Agent delivery uses the same lifecycle boundaries through
`AgentTurnDriver::with_input_receipt`. A thin optional observer is carried
through the existing `drive_chat_turn -> drive_prepared_chat ->
drive_chat_inner` path. The framework publishes `Accepted` immediately before
calling the Agent stream API and `Drained` only after the concrete Agent has
placed the input in model context. AgentRouter persists the same canonical
`MailboxAccepted`, `Drained`, and `TurnSettled(outcome)` facts; it never infers
drain from output, EOF, or terminal settlement.

`EffectStarted` remains the application pre-effect fact for an exact message
attempt and turn. Framework `Accepted` is persisted separately as
`MailboxAccepted`, and real context consumption becomes `Drained`. A terminal
with `drained: false` remains typed and non-retryable when the effect outcome is
unknown. A drained attempt remains non-replayable across restart until boot or
the owning foreground turn supplies `TurnSettled`.

## Consequences

- One framework receipt is the real-time lifecycle authority; app events are
  rebuildable facts and do not create a second mailbox or reducer.
- There is no mailbox-delivery alias event or status. `status` is a read-only
  value derived from phase and rejection detail; it cannot own lifecycle state.
- New generated TypeScript types mirror the typed phase/outcome and event
  vocabulary.
- Live and cold delivery now share real accepted/drained/turn-settled receipt
  boundaries while retaining the replayable compatibility event vocabulary.
- `echo-website` is a static public site for framework/product guidance and has
  no Tauri TaskRun control contract; website synchronization is therefore not
  applicable to this app-internal receipt change.
