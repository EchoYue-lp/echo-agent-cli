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

Cold Conversation Agent delivery remains conservative until the shared chat
driver exposes its initial `TurnInputReceipt` to the AgentRouter adapter. The
current cold path may only publish its existing `Injected`/`Delivered`
compatibility projection after the driver terminal; it must not claim an early
drain.

## Consequences

- One framework receipt is the real-time lifecycle authority; app events are
  rebuildable facts and do not create a second mailbox or reducer.
- Existing event names and wire status remain replayable for old history.
- New generated TypeScript types mirror the typed phase/outcome and event
  vocabulary.
- Cold delivery parity is an explicit follow-up gate for the next F1 slice.
- `echo-website` is a static public site for framework/product guidance and has
  no Tauri TaskRun control contract; website synchronization is therefore not
  applicable to this app-internal receipt change.
