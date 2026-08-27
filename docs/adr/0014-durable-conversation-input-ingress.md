# ADR 0014: Durable Conversation Input Ingress

- Status: Accepted
- Date: 2026-08-27

## Context

EKO had four different follow-up behaviors. GUI queued raw inputs durably in
`ChatEventLog`, TUI and CLI REPL kept process-local `VecDeque` values, and
channel steering asked the user to retry. GUI also removed a durable item once
a new turn was merely started, while the frontend removed it a second time.
Those paths could lose an input before model-context drain, replay an ambiguous
attempt after restart, or report different behavior by surface.

The repository-wide search covered `ChatEventLog` queue facts and retention
pins, `PreparedUserTurn`, `ConversationStore`, `AgentRouter`,
`ForegroundTurnControl`, TUI `queued_turns`, REPL `ReplTurnQueue`, Tauri queue
IPC, and channel `/steer`. Only `ChatEventLog` already supplies the required
conversation-scoped durable stream, sync-data barriers, retention pins, replay,
and one reducer. `ConversationStore` is transcript/metadata persistence, while
`AgentRouter` is a different cross-workspace Agent mailbox. Neither is a valid
second authority for user follow-ups.

Claude Code's official [Interactive mode documentation](https://code.claude.com/docs/en/interactive-mode#queue-messages-while-claude-works)
documents the product behavior used as reference: input submitted while Claude
works is queued rather than interrupting the turn; messages may enter the same
turn after current tool calls, remaining entries become separate subsequent
turns, interrupt keeps the queue, and users can take queued entries back for
editing. EKO adopts the queue/steer/next-turn behavior but adds local durable
receipts because it must recover across application restart. The configured
OpenAI official-document retrieval was unavailable in this environment, so no
OpenAI behavior is claimed or used as evidence for this decision.

Temporal's official workflow documentation treats ordered event history as the
recovery authority and does not repeat an activity whose result is already in
history; its retry policy also separates non-retryable failures. NATS JetStream
likewise distinguishes delivery from acknowledged consumption and documents
that a lost acknowledgement can cause redelivery. EKO adopts those lifecycle
boundaries and the rule that known-consumed work cannot return to the execution
frontier, while deliberately avoiding a workflow server, broker, distributed
lease, or network acknowledgement protocol. References:
[Temporal workflows](https://docs.temporal.io/workflows),
[Temporal retry policies](https://docs.temporal.io/encyclopedia/retry-policies),
and [JetStream delivery and acknowledgement](https://docs.nats.io/learn/jetstream/delivery-and-acknowledgment).

## Options

1. Keep each surface queue and align their UI behavior. Rejected because four
   authorities still disagree after restart and cannot share exact receipts.
2. Put follow-ups in `ConversationStore` or `AgentRouter`. Rejected because this
   overloads transcript persistence or creates a second mailbox and reducer.
3. Extend the existing `ChatEventLog` conversation-stream reducer with typed,
   revisioned input lifecycle facts and expose a stateless application service.
   Selected because it replaces, rather than parallels, current queue state.

## Decision

### Layering

- **Framework mechanism**: `AgentSteerReceipt`, `TurnInputReceipt`,
  `AgentTurnDriver`, and foreground turn settlement remain the generic sources
  of mailbox acceptance, model-context drain, and typed turn outcome. No EKO
  queue policy moves into `echo-agent`.
- **EKO application policy**: the existing `ChatEventLog` conversation stream,
  journal, durability barrier, retention pins, and reducer are the sole durable
  input authority. Its lifecycle is `Persisted -> AttemptStarted ->
  MailboxAccepted -> Drained -> TurnSettled`, with `Deferred` and
  `RecoveryRequired` branches and explicit cancellation.
- **Surface adapters**: GUI, TUI, CLI, and channel provide rendering, HITL, and
  interaction policy only. They call `ConversationInputService`; they do not
  own a queue or infer drain/settlement from output, EOF, or UI status.

`ConversationInputService` is stateless except for an `Arc<ChatEventLog>`
dependency. It owns no map, `VecDeque`, journal, reducer, mailbox, task, or
driver. `PreparedUserTurn` remains the only input-resource-to-Message merge
point when a claimed input is dispatched through the existing foreground
driver.

Each input is addressed by workspace, conversation, input id, and revision.
Each effect attempt adds an ordinal, unique attempt id, and exact turn id.
Settlements must match that complete identity. Reusing an input id with the
same canonical payload returns the current receipt without another fact;
different content is an identity collision. Queue reorder uses a compare-and-
set queue revision.

Surface source (`Gui`, `Tui`, `Cli`, or `Channel`) participates only in the
stable scoped input-id derivation. `InteractionMode` is deliberately absent
from ingress: dispatch reads the current capability/policy, and F4 will delete
the old mode enum. TaskRun resume is also not a ConversationInput payload; it
continues through the existing revisioned TaskRuntime continuation/wakeup
authority. Durable cursor/watch delivery is deferred to Iteration 6 and is not
smuggled into P1 metadata.

An explicit pre-effect steer rejection may become `Deferred`. A turn that
settles with `drained = false` may remain dispatchable. `AttemptStarted` without
a conclusive receipt becomes `RecoveryRequired` after owner loss and is never
automatically replayed. Once `Drained` is durable, the input leaves the pending
frontier permanently even if its later turn settlement is failed, cancelled,
or dropped.

For correctness, a drained, settled, or cancelled input retains an exact
terminal tombstone at its latest lifecycle fact. The tombstone carries the
payload hash and exact identity, so duplicate/collision behavior survives
journal pruning and reopen without retaining pre-terminal history. This P1
implementation does not claim a bounded tombstone set: compacting terminal
tombstones into a bounded checkpoint is explicitly deferred to final F7
retention/performance optimization and its 10k/100k plus soak gates.

The service receipt observers persist real mailbox acceptance and context
drain, and may observe a framework terminal directly. Otherwise the existing
foreground owner retains the exact `ConversationInputAttempt` and calls
`ConversationInputService::settle_attempt` before releasing its lease. The
attempt carries a process-local atomic known-drained bit shared by observer and
terminal projector; journal-cache eviction cannot turn consumed input back
into replayable work. Retrying the exact terminal projection is idempotent.

Active steer observers are registered on the exact existing
`ActiveForegroundTurn` entry. The entry closes observer admission and joins
every registered observer before durable terminal projection and lease release;
this is supervision metadata on the foreground owner, not another driver or
store. `drive_foreground_chat_with_ingress` provides the initial-input adapter:
observer failure changes the real foreground outcome to failed, and the same
owner retains and retries durable terminal debt before publishing settlement.
Dropping a surface waiter does not drop a task already accepted by the existing
foreground supervisor.

Live steering registers its receipt observer and a
`ForegroundTerminalProjector` atomically through
`supervise_input_lifecycle_scoped` before performing the steering effect. This
covers TaskRun/resume paths that have no initial-input callback. Settlement
closes registration, joins every observer, converts observer failure into the
single final Failed outcome, then retries the initial durable terminal callback
and all registered projectors independently with bounded backoff. If durable
projection remains unavailable, the exact foreground entry stays active with
typed terminal debt and closed input admission; shutdown returns that debt
without hanging or admitting a conflicting turn.

Application boot reconciliation scans the existing ChatEventLog conversation
streams. Owner loss at `AttemptStarted` or `MailboxAccepted` appends one
self-contained `Cancelled` terminal carrying the exact attempt and recovery
reason; owner loss after `Drained` appends exact `TurnSettled(Dropped,
drained=true)`. A runtime observer error first records `RecoveryRequired`; the
same foreground owner then converts that exact attempt to self-contained
`Cancelled`. Production and tests no longer expose the old raw queue variants,
writers, readers, or surface reducers.

The GUI, TUI, CLI REPL, and channel adapters use this service and the shared
foreground driver. Queue command names at the Tauri boundary remain product
commands, but they return the typed frontier or receipt and do not call a
legacy queue store. Attachment submission reuses the existing application
staging path with backend-enforced per-file and aggregate durable payload
budgets.

## Consequences

- One durable input and receipt authority is shared by every interactive
  surface without a new runtime.
- Ambiguous attempts fail closed instead of being silently lost or replayed.
- Existing journal retention and conversation deletion policy apply to queued
  inputs without SQLite or a new store.
- Terminal tombstones currently pin their latest self-contained journal fact
  without a bounded count. F7 must compact them into a bounded checkpoint and
  validate the bound with the deferred 10k/100k and soak gates; P1 prioritizes
  exact duplicate/collision safety across prune and restart.
- Every interactive surface consumes the same typed receipt phases; surface
  renderers do not own lifecycle state.
