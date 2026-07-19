# M10 Surface Parity Closeout

## 1. Goal

M10 is the final planned milestone of the current M1-M10 iteration. It does not
add another runtime or state machine. It closes the remaining projection and
interaction gaps across GUI, TUI, CLI, IM channels, and cron-triggered runs,
then turns parity into a permanent test gate.

The required scope is:

- Chat / Task / Auto mode selection and execution-path reporting.
- Plan create, edit, execute, pause, resume, and cancel.
- Foreground, background, and cron runs.
- Subagent / Team lifecycle, result, artifact, verification, and failure.
- Approval, free-form input, and selection HITL.
- Memory, skills, MCP, Browser, attachments, and multimodal input.
- Tool streaming, timeout, cancel, retry, structured failure, and artifacts.
- Provider usage, cache, compression, and protected-context diagnostics.

The low-priority candidates in `MASTER-PLAN.md` remain outside this iteration:
Feishu webhook-specific multimodal transport, unproven `isolated.rs` cleanup,
unused evolution hook sites, and Hosted Agent Service. They are not M1-M9
regressions and do not block M10.

## 2. Industry References

The closeout follows two official client protocols:

- [OpenAI Codex app-server](https://learn.chatgpt.com/docs/app-server) models a
  client session as Thread -> Turn -> Item. All rich clients consume the same
  JSONL event protocol (`item/started`, deltas, `item/completed`) and converge on
  `turn/completed`; clients render the facts differently instead of rebuilding
  lifecycle state.
- [Claude Code headless mode](https://code.claude.com/docs/en/headless) exposes
  the same agent loop through `stream-json`: messages are NDJSON events, the
  final `result` record carries terminal/session facts, subagent records retain
  parent tool identity, retry progress is explicit, and clients are expected to
  feature-detect capabilities and tolerate new event values.

The shared pattern is one versioned event stream with stable identity and an
explicit terminal record. EKO already has framework `EventEnvelope` and
application `ExecEvent`; M10 connects both through one application transport
instead of inventing a new framework protocol.

## 3. Framework / Application Boundary

All M10 implementation belongs to `echo-agent-cli`:

- `echo-agent` already owns the reusable `EventEnvelope`, `AgentEvent`, tool
  failure, artifact metadata, HumanLoopProvider, and run trace contracts.
- EKO owns Chat / Task / Auto policy, TaskRuntime `ExecEvent`, GUI/TUI/CLI/channel
  rendering, per-surface commands, and product capability evidence.
- Therefore M10 adds an EKO `ChatDriverEvent` transport, a test-owned
  capability matrix and wire-contract checks, and surface adapters. It does not
  add product surfaces or TaskRuntime concepts to the framework.

## 4. Current Gaps Confirmed by Code Audit

1. `ChatSink` only requires agent events. Turn status, execution path,
   interrupt, and TaskRuntime trace methods default to no-op / `None`, so a new
   surface can compile while silently dropping facts.
2. TUI approval is interactive, but free-form Input is auto-approved and
   Selection silently chooses the first option.
3. Channel agents are created with an empty HITL dispatcher and therefore
   reject every request; the non-streaming channel path also bypasses
   `drive_chat`.
4. Channel `ChatResources` omit the existing review/memory layer manager, so
   completed autonomous-run memory writes become no-ops.
5. CLI `/remember` and `/forget` print success without touching the installed
   memory store. CLI interaction mode is fixed to Auto.
6. GUI/TUI/CLI/channel reducers do not all cover budget, guard, memory recall,
   safety notice, parameter error, chart fallback, lifecycle, and TaskRuntime
   side events.
7. CLI memory preview slices UTF-8 text by byte offset.
8. TUI `/cron` sends prose to the agent instead of controlling the scheduler.
9. Cron stream setup/terminal errors can leave a run reported as completed.
10. Channel attachments reach the main agent but are not persisted as refs for
    TaskRuntime subagents.

## 5. Target Contract

`drive_chat` emits a single `ChatDriverEvent` enum:

- `Agent(EventEnvelope)` for framework/model/tool events.
- `Execution(ExecEvent)` for TaskRuntime and subagent facts.
- `TurnStatus`, `ExecutionPath`, and `Interrupt` for product turn facts.

`ChatSink` has one required exhaustive entry point. Helper sinks bridge
`ExecEvent` into this entry point for task-local and framework external run
contexts. There are no default no-op lifecycle methods and no optional live
TaskRuntime sink on an interactive surface.

Contract tests serialize the shared interactive event and the durable cron
`RuntimeTaskEvent`, then assert that lifecycle, error, identity, and artifact
facts survive the wire format. Interactive rendering remains surface-specific;
cron persists the same facts for later inspection.

The code-owned capability matrix requires concrete evidence for every required
capability on all five entry classes. Support may be native UI, terminal/text
fallback, a direct command, or the shared agent tool path; an empty entry fails
tests.

## 6. Interaction Closeout

- TUI uses one pending HumanLoop card for Approval, Input, and Selection. It
  never fabricates an answer.
- Each channel sender session owns a pending HumanLoop provider. The next
  inbound message resolves approval/input/selection before starting another
  turn. Text commands expose interaction mode and direct run operations where
  a graphical control is not available.
- CLI mode is mutable through `/mode chat|task|auto`; `/remember` and `/forget`
  use the installed layered memory manager, with the raw Store as fallback.
- Channel turns receive the existing `ReviewIntegration` layer manager.
- Channel attachments are staged as the same durable `AttachmentRef` used by
  GUI/TUI so TaskRuntime subagents can reconstruct the multimodal message.
- TUI controls the real scheduler directly; cron failures persist a Failed run
  and return an error instead of being reported as success.

## 7. Delivery Slices

To keep reviewable diffs, M10 is split by responsibility:

1. Unified product event transport, capability matrix, and wire-contract tests.
2. TUI/channel HITL and channel memory/driver parity.
3. CLI real memory/mode commands and remaining event render coverage.
4. Frontend reducer coverage, full feature matrix, browser QA, archive, and
   commits.

Each slice must compile before the next begins. The final gate is the complete
Rust workspace/feature matrix, frontend type/tests/build/format checks, browser
desktop inspection, `cargo clean`, GPG-disabled commits, and
`MASTER-PLAN.md` archival.
