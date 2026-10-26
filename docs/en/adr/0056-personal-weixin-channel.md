# ADR 0056: Personal Weixin authentication, direct chat and cron results

## Status

Accepted design, source implementation candidate. The original Core epoch 4 on
Framework F863 passed its library-only channels check with zero warnings; the
subsequent long-control source is uncompiled and has no native acceptance.
The original-pump no-stream admission mechanism
targets the 32-slot/strict-cursor/HITL backlog counterexample, but pressure
verification has not run; the complete Outcome is unqualified. Long native-control
ownership is wired in source and still requires actual consumer verification.
0056 is reserved by the central owner; existing Domain0054 and Project0055 remain authoritative.

## Context

EKO is a local personal assistant with a GUI entry. IM/cron retain the original
Agent, TaskRun, HITL and file authorities. ChannelRuntime/ChannelManager own
producers and stop/join; ConfigState owns config CAS/persistence; original
SchedulerRunner/CronTaskStore/TaskRuntime journals own occurrences, runs and
results. Account, phone user and destination must remain explicit: a displayed
project or replacement login cannot take over admitted work.

The first-party reference is [Tencent/openclaw-weixin 874637a0dbc24050f72d1385add4fc6170a2d726](https://github.com/Tencent/openclaw-weixin/tree/874637a0dbc24050f72d1385add4fc6170a2d726),
version 2.4.10 under its [MIT license](https://github.com/Tencent/openclaw-weixin/blob/874637a0dbc24050f72d1385add4fc6170a2d726/LICENSE).
Its QR → local credentials → authenticated poll → context-reply layering is the
reference. OpenClaw host integration is not an EKO SDK, and its fixed source does
not qualify EKO's own client authorization or actual phone receipt.

## Options

1. Install OpenClaw Gateway or inject desktop WeChat. Rejected: adds another
   host/ownership boundary.
2. Create a Weixin inbox, scheduler, TaskRun FSM or SQLite store. Rejected:
   duplicates existing authorities.
3. Framework protocol plus optional ingress observation; EKO auth/peer/UI and
   notification policy, thinly adapting original journal/ledger. Selected.

## Decision

Original ChannelRuntime gains a bounded auth slot retaining the same challenge
producer, cancellation and actual join. Begin/refresh/cancel/commit use original
operation boundaries without holding the lock through network polling. Confirm
commit checks challenge, revision, instance and credential generation. Complete
confirmed identity and successful owned-config save/CAS precede authorization
publication. Already-bound supplies no new credentials or automatic online claim.
Numeric codes, QR payload, bot tokens and context stay out of ordinary DTOs,
Debug, prompts and logs. QR encoding is local. HTTPS and disabled automatic
cross-origin redirects protect local credential correctness, not full-auto policy.

One active account admits only its scanner's direct text at the original EKO
boundary before session/Agent effects. Groups/media/mixed media do not execute
remaining text. Canonical account/peer codec, persisted credential generation and
original session incarnation/fence remain distinct; replacement cannot take over
old requests. Configured/authorized, authenticated-poll Ready, send Accepted and
phone receipt remain separate. `-14` closes new receive/send admission and
requires reauthentication. Forget clears local login/protocol state without
erasing history/unsettled obligations or claiming server revocation.

Optional ingress receipts originate at durable input or control journals, never
lazy-stream return. Original ChatEventLog records control claim/effect/result;
uncertain effects retain RecoveryRequired and forbid replay without another inbox.
The original effect owner reports a typed result before rendering: confirmed
success completes the same control ledger; syntax/no-effect rejection retains its
original identity; confirmed no-admission defers that exact payload and withholds
ACK; partial/unknown effects durably settle Unknown before RecoveryRequired.
Management commands `/trace`, `/analysis` and `/papers` now retain the same
native execution result before their legacy text projection. Parse rejection, failed read,
confirmed pre-admission refusal and uncertain mutation remain distinct. Read errors
retain a rejection result; they do not pretend that their read was never admitted. Only
original business success plus physical foreground completion yields Applied;
a failed foreground remains Unknown. ProductData `Admission` proves refusal,
whereas `OwnerClosed` is a lost post-admission observation and cannot authorize
mutation replay. Public text renderers call the same executor once.
Rendered Ok/EOF never supplies effect proof. Supervised operations retain the
same lease even after their output observer is dropped. Native channel commands
explicitly declare snake_case, matching the existing real endpoints/bridge path.
Original response producers capture context/generation and preserve outbound
fences. The original bounded sender returns Accepted/Rejected/Unknown; missing
ret, timeout and damaged receipts are not success or automatic replay permission.
QQ/Feishu retain existing non-opt-in paths.

The original factory passes the exact WeixinDeliveryHandle. Queued original
input-pump routes retain only receiver, exact-message context Option and original
fenced-template data, without a response task, SessionStreamReceipt or delivery
permit. After submit, original route registration and starting/notifying the
original pump, the no-stream admission returns `Result<()>`. After claim, the same single
pump concurrently polls original foreground/HITL and reply consumer; actual
sending awaits the original bounded producer. Send failure still drains the
renderer and observes the original driver. Active inputs and explicit Steer
retain tracked live injection. Durable input plus original supervised-observer
ownership yields an immediate fenced persistence ACK, explicitly awaiting
processing, without awaiting drain or adding an output waiter. The original
observer records actual drain/recovery; deferred/no-active cases use the pump.
An RAII admission in the same slot spans submit through route registration so a
concurrent pump cannot mistake in-publication input for an unroutable orphan.
Framework `bind_outbound`
captures a fence without send admission; `has_pending_work` observes original
retained work to protect idle expiry. Existing Store CAS fixes per-message
context including explicit None; `capture_message_context`/`send_with_context`
never fall back to the latest token.
Session eviction uses only retained input-pump work and active foreground turns;
it never reaps the service `JoinSet` from this observation. Coordinator control
and close boundaries own that reaping.


The selected A+A2 host contract uses `SessionHandler::with_durable_ingress`,
fallible `bind_durable_ingress(service_cancel)` and no-stream
`admit_with_ingress(message, cancel, receipt)`. The service child is bound before
admission; stop cancels claims and exact original foreground roots before
physical join. Idle expiry observes both pump work and supervised control owners.

One optional `ConversationInputPayload.channel_binding` freezes v1 channel,
account/credential generation, exact provider message/peer, original opaque
workspace product-data generation, required tagged Absent/Reference context,
and Chat/Resume intent. Resume retains the complete original expected identity,
including central Project binding, and its separate driver-continuation switch.
The original Domain-aware digest is unchanged for None; bound digest includes
the address, text, attachments, Domain selection and every binding field.
Only plain non-secret reference metadata is persisted, never tokens or fences.

The original full journal query discovers saved provider identity before default
workspace setup. Recovery initialization belongs to the original ChannelRuntime
start/reconnect task group and awaits actual same-epoch authenticated-poll Ready.
It uses the same SessionHandler and a fresh fence. The same atomic claim and
queue revision recheck strict Persisted, all attempt fields absent, undrained,
complete pinned history and confirmed durability; Deferred or a missing Started
record is not proof. Claimed/effect/send/old-owner uncertainty remains
RecoveryRequired. Registry generation is checked inside original publication
and scoped runtime leases before claim/effect/send, and the original provider
validates exact message-context association, including explicit absence.
No context/token/latest fallback or current GUI focus can replace saved DATA.

Recovery admission has one owner per transport mutation epoch and persisted
ConversationInputIdentity. Concurrent initialization tasks may observe the same
DATA, but only the first claim may enter SessionHandler; duplicates retain the
original journal fact and do not send or invoke the model again. Pending-work
observation is side-effect-free: `has_pending_work` must not reap or poll the
coordinator's service `JoinSet`. Reaping completed owner tasks remains on the
explicit coordinator/close boundary. This prevents recursive observation while
SessionHandler prepares an admission and preserves one model/send result.

Resume uses the original Continuation bound-ingress driver or Planned
`launch_task_run_resume`/wait, preserving the original pool execution lease and
Domain policy. Its original control EffectStarted is confirmed before launch.
Planned admission projects a typed original DriverAccepted fact; it never fakes a
ReAct mailbox receipt. Runtime retirement/registration reuses the same original
gate before foreground claim. Queued inputs hold only DATA, with no per-input
output or cancellation task.

Framework ADR0007 supplies the original verified committed-prefix guarantee.
Only the final unterminated incomplete frame of the active segment is discarded;
complete corrupt frames, closed-segment tails and identity/sequence errors reject
open. InputLifecycle requires confirmed SyncData before claim returns successfully,
and the pump starts execution only from that successful claim. A complete degraded
claim is still folded and blocks replay. Consequently an uncommitted incomplete
tail does not itself make proven Persisted DATA permanently unrecoverable.
App uses the original open/replay/pins and atomic claim barrier; it adds no length
heuristic, repair marker, side proof file, validator or physical journal. This is
bounded by the original supported journal failure model, not arbitrary external
history deletion/rollback, and is not compiled/native acceptance.


Notification policy is EKO `channels.weixin.notifications`, selected by exact
definition/workspace. Original occurrence admission freezes its target in the
same TaskRuntime journal frame. Duplicate occurrences reuse original bindings,
including historical None, without inferring today's config. Target includes
account, credential generation, peer, opaque context reference, workspace,
channel conversation and definition; original occurrence/run supply execution
identity. Actual completion results/artifact references use existing
DeliveryLedger and `apply_prepared_with` to commit typed delivery events in the
same physical TaskRuntime journal. Confirmed durable claim/EffectStarted precede
network effect; in-memory projection is not persistence. A crash between
completion/intent may rebuild an unstarted intent. Existing effect without a
receipt remains Unknown, never re-claimed. Delivery failure does not change task
outcome or rerun its prompt.

ApplicationServices still constructs the original scheduler before publishing
Arc<AppState>/ChannelRuntime. A thin Weak binding waits through Notify inside
the original callback future, listening to the same scheduler cancellation.
Startup therefore cannot fail an ordinary occurrence solely before binding;
there is no extra waiting task or scheduler loop. Send validates committed
account/generation, enabled and frozen context, without substituting latest
tokens or the current GUI workspace.

After original configured-channel startup, the existing ChannelRuntime invokes
AppState recovery over global and registered workspace journals. The same
RunCreated frame freezes the full OccurrenceIdentity beside its destination.
Completed runs can rebuild an absent or unstarted notification intent through
the original driver and ledger. Physical workspace leases survive abandoned
observers until original delivery settlement. Accepted never resends and started
Unknown never reclaims; prompt execution and TaskRun terminal state are unchanged.
Missing identity and persistence failures remain visible pending diagnostics.

A local account, destination or context failure before HTTP admission retains its
original reason as notification `Failed`, prefixed `(local not sent)`, without
inventing a provider rejection code. Ambiguity after sender admission must return
typed `Unknown` with its sanitized reason and no automatic resend. Neither
changes the completed TaskRun.

## Consequences

The channel page gains a QR card; the scheduled-task page configures the
original definition and history reads independent notification receipts only
for resolved original runs. Accepted is not phone receipt/read status. Unknown
no-context, restart, window and quota behavior receives no proactive-delivery
guarantee. Dependency/lock changes require the central author's exact identity
and risk checks, without extending old acceptance, SDK inventory or build pins.

The previous path could occupy 32 ordinary response slots waiting for output,
blocking the strict cursor frontier and leaving later `/reply` in the backlog.
The original-pump data route and short admission stream target this structural
counterexample, beyond merely adding control slots and without another inbox.
Greater-than-32 plus later `/reply` pressure, compilation and native checks remain
unrun. This is not verified closure; the complete Outcome still needs those
checks. Static source, fixtures, builds, consumers, real
GUI/phone, main CI and release remain separate. The website is mirrored only by
the central owner after approved source freeze; no deployment occurs here.

## Testing

Management regression definitions use real SessionHandler ingress for invalid
analysis arguments, a closed native analysis supervisor, absent trace storage,
closed ProductData admission, actual native create/delete failures and successful
paper creation. After original journal reopen, the same provider ID must retain
its rejection/recovery/success; a restored source file detects accidental repeated
delete, and source count detects repeated create. All remain UNRUN.

Control fixtures now close/reopen the real SessionHandler and original journal
before replaying an Unknown result and a successful read control. The 24-input
fixture traverses real SessionHandler and durable-submit entrances; it observes
an actual tracked mailbox input while ordered successors remain in the original
frontier, then checks original settlement/recovery after physical cancellation.
It does not claim 24 simultaneous tracked mailbox admissions. The separate
40-input fixture holds the original pump election while all inputs and phone
reply traverse SessionHandler. Provider cursor/retained-stream pressure belongs
to the separate framework poll fixture. All are unexecuted test definitions.

Source contains test definitions for protocol states/redaction, cursors, control
replay, destination freezing, physical journal durability, Unknown recovery and
GUI identities. Definitions are not passing evidence. Cover the actual bounded
32-slot saturation plus later `/reply` path and complete applicable compile,
test, feature, example and exact dependency gates. Framework demo38 retains its
actual QQ/Feishu example: default AgentChannelHandler lacks durable ingress
opt-in, so no fake ACK is added. Comments do not qualify example compilation.

This candidate has no compilation or native acceptance. Only the user performs
final real GUI authentication, numeric confirmation, direct/HITL messages,
restart and scheduled-result phone receipt checks. Development fixtures do not
connect real accounts. Mocks, HTTP200, webhook finished labels, screenshots and
source presence cannot replace platform acceptance or actual receipt. See
[configuration](../configuration.md), [task panel](../guides/gui-task-runtime-panel.md)
and [ADR0053](0053-im-cron-gui-control.md).

Additional A2 test definitions cover unchanged None/Domain digests, mandatory
context/generation, address/binding tampering, lossless context projection,
clean journal reopen and single claim, previously claimed Deferred exclusion,
service cancellation before atomic claim, safe uncommitted incomplete-tail recovery through an unrelated boot scan and
complete corrupt-tail isolation, and discovery beyond 128 cached streams / 4096 replay
records. Existing 40/24 actual SessionHandler definitions now use no-stream
admission and exact canonical sender identities. They seed only explicit absent
protocol associations through the injected Store. All definitions remain
uncompiled/unrun, and do not replace the original physical owner
contract or engineering qualification.

Control response adaptation now returns typed immediate response DATA or a
legacy stream internally. Durable admission never polls a legacy opaque stream
to infer receipt or output. Original supervised compact/reset/extraction and
management owners send their own final text through the same delivery handle;
immediate controls send within their short invocation. The original provider
message ID is retained in reply_to for ordinary, restored and control replies.

Native-control source now hands reflection and workflow execution to the original
coordinator service JoinSet without adding a foreground gate. Browser confirmation
and the other Extensions retain their existing foreground lease and execute the
same operation body in the original foreground supervisor. Legacy surfaces await
that same body directly. Admission returns without waiting for the native effect;
the original owner keeps its control journal lease, reports only its real result,
and sends its own response. Task registration alone never publishes ControlHandled.
Idle observation reaps the original task group and retains its failures; Stop joins
that same group, including after a close observer is dropped. No new queue,
dispatcher, store, public API, approval service or native control policy is added.
New real SessionHandler test definitions cover held reflection, original workflow
Graph completion and browser consequential confirmation; none has run. Full A2
still requires real Ready-to-driver-to-Accepted-send consumer fixtures and both
positive/negative crash cases, not journal-only or fake sender evidence.

For supervised controls, the original observer keeps an already-confirmed journal
disposition until that same owner's response future settles. Response completion,
drop and EOF supply no fact. This preserves the existing pending frontier under
sender backpressure without another permit, gate or queue. Native owners mark
response ownership before polling their business body, including when a task runs
before its admission caller resumes. Held-response and no-fact regression sources
remain unexecuted.

Ready initialization delegates live-route observation and never-claimed proof to
the same private admission path; a separate early proof must not downgrade a
current live owner. Concurrent publication of the same verified input reuses its
original reply slot. Known completed/cancelled history stays terminal both during
initialization and provider replay. Consumer tests assert the completed outcome on
the next cold restart, not merely zero model calls. After the original atomic
claim, the pump retains that route until the existing reply-publication admissions
settle; a competing initializer cannot republish behind a consumed route. This
extends the original publication barrier without another queue or lifecycle.
Cold recovery test sources race two original initialization tasks and remain
unexecuted.

The original service wrapper cancels its bound child and joins the same coordinator
recovery/pump task group even outside the managed Stop caller. Inner session close
withdraws its original IM HITL before awaiting its exact foreground. Cancelled
close observers retain the original task group for retry. Start captures default
runtime and opaque product-data generation together; a same-ID relink/recreation
cannot silently replace that default binding. Existing live routes retain their
typed Deferred semantics; only reconstructed routes use the strict A2 proof.
Discovery failure is not a negative identity lookup, and blocked addresses retain
their facts without hiding independent safe streams. These regression definitions
remain unrun.
