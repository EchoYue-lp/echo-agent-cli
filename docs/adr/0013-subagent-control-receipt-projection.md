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
retains `SubagentGuidanceDelivered` as a compatibility projection whose
boundary is only mailbox acceptance. It additionally records typed
`SubagentGuidanceMailboxAccepted`, `SubagentGuidanceDrained`, and
`SubagentGuidanceSettled` events keyed by the exact command identity.

`SubagentControlReceipt.status = delivered` therefore means accepted by the
framework mailbox for compatibility; `phase` carries the durable application
boundary and `outcome` is present only at owning-turn settlement. The
application never infers drain from rendered text, stream EOF, or foreground
status. GUI/TUI/CLI/channel active steer calls use the tracked framework API and
retain their existing FIFO fallback on rejection or terminal-before-drain.

Cold Conversation Agent delivery uses the same lifecycle boundaries through
`AgentTurnDriver::with_input_receipt`. A thin optional observer is carried
through the existing `drive_chat_turn -> drive_prepared_chat ->
drive_chat_inner` path. The framework publishes `Accepted` immediately before
calling the Agent stream API and `Drained` only after the concrete Agent has
placed the input in model context. The AgentRouter writes its existing
`Injected` compatibility fact only for that real drain; it never infers drain
from output, EOF, or terminal settlement.

`InjectionStarted` remains the application effect-started fact for an exact
message attempt and turn. It is not renamed to mailbox acceptance. Framework
`Accepted` is observed inside the typed receipt, while the compatibility
AgentRouter journal persists `Injected` at the drain boundary. A terminal with
`drained: false` is persisted once through the existing `Failed` event. A
drained attempt remains `Injected` across restart until the owning foreground
turn supplies its real terminal settlement, so recovery cannot reclaim or
replay the input.

## Consequences

- One framework receipt is the real-time lifecycle authority; app events are
  rebuildable facts and do not create a second mailbox or reducer.
- Existing event names and wire status remain replayable for old history.
- New generated TypeScript types mirror the typed phase/outcome and event
  vocabulary.
- Live and cold delivery now share real accepted/drained/turn-settled receipt
  boundaries while retaining the replayable compatibility event vocabulary.
- `echo-website` is a static public site for framework/product guidance and has
  no Tauri TaskRun control contract; website synchronization is therefore not
  applicable to this app-internal receipt change.
