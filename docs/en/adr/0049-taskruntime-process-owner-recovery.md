# ADR 0049: Recover TaskRuntime owners after process loss

## Status

Accepted

## Context

`PhysicalOwnerIntent` fences duplicate TaskRun execution while an original
driver or tool has not physically settled. The root's existing
`RootTransactionAuthority` holds an exclusive OS file lease, shared through one
canonical in-process registry. After a crash the kernel releases that lease,
but a journaled intent previously had no process identity. A new process could
own the root and still leave the old run permanently Running or its paused
command cells permanently active. A crash between terminal status and intent
acknowledgment could also retain a fence on a completed run.

The existing framework [fs2 implementation](https://docs.rs/fs2/latest/src/fs2/unix.rs.html)
uses `flock` on Unix. The [flock(2) lifetime rule](https://man7.org/linux/man-pages/man2/flock.2.html)
releases the lock when all related file descriptors close. Request cancellation,
a timeout, or a guessed PID lifetime cannot prove that ownership ended.
Checkpointed execution systems such as [LangGraph persistence](https://docs.langchain.com/oss/python/langgraph/persistence)
also separate recovery from replaying side effects. This decision uses EKO's
existing lease and recovery rules; it does not infer a LangGraph epoch design.

## Options

1. Clear every owner intent at boot. Rejected: another store in the same
   process can observe the same root while its real driver is still alive.
2. Use PID checks, TTLs, or a second owner store. Rejected: these add guesses or
   duplicate authority rather than using the exclusive lease already held.
3. Bind new intents to a process nonce and reconcile only previous-process
   admission fences under the existing root authority (chosen).

## Decision

- This is EKO's application file-authority and boot projection policy. The
  framework keeps its existing generic lease, journal and task recovery APIs.
- The existing root authority exposes a process-wide UUID initialized once.
  All stores and roots in that process use it, including dropping and reopening
  a root. The UUID identifies an incarnation; holding the OS lease supplies the
  ownership proof. Neither is a physical-completion receipt.
- `begin_physical_owner_intent` is the sole new-owner journal boundary. It
  overwrites any supplied epoch with the current root owner's epoch before
  admission. Driver and standalone tool producers retain that exact identity.
  Physical completion continues to require the original matching intent.
- Only boot reconciliation retires a valid foreign-process intent. Missing
  epochs identify journals from before this producer invariant: a current
  producer cannot create them, and the exclusive root lease prevents a
  competing old process from still owning that root. Empty or invalid UUID
  epochs fail closed as malformed evidence.
- Retirement appends `RecoveryResolved` with the original intent unchanged,
  reason `owner_process_interrupted`, and the recovery process epoch. It never
  sets `physical_terminal_observed`. Current-process intents keep their fence,
  including repeated boot calls and same-process store reopening.
- Running and paused runs then use the existing interruption recovery. Unsafe
  tool/Subagent side effects remain blocked for inspection; command cells are
  recorded interrupted; no model or tool is replayed by reconciliation. Other
  run statuses only retire obsolete admission debt and keep their committed
  business status. They do not increase the recovered-work count.
- Owner retirement and Running/Paused interruption recovery share one journal
  batch. A failed recovery cannot expose an unfenced run before unsafe effects
  have durable blockers. Paused runs retain their original pause reason and
  existing cell terminal events; recovery grants no new auto-resume eligibility.
- Accepted exact-claim cancellation or pause is settled by the framework's
  existing interruption-first resolution. Boot stores its complete execution,
  including the cleared claim/request, without losing the accepted disposition
  or incrementing retry counters. Unsafe effects retain a separate inspection
  blocker even if the task was already cancelled or completed.
- Admission, cancellation, and retry do not gain a reset path. The append-only
  journal remains the sole authority. A failed append is retried from that
  journal, including a crash during the atomic recovery append or its replay.

## Impact and validation

This refines ADR 0015's original-owner fence by distinguishing current-process
execution from process loss. It adds no TaskRun status, scheduler, public API,
SQLite store, or wire contract. No example changes are needed. The website has
no separate description of this private recovery policy; its source inventory
must be regenerated from a reviewed committed CLI revision during publication.

Regressions cover a real registered driver killed in a subprocess, rejection
while the competing process still owns the root lease, old journals without an
epoch, unchanged same-process fences, producer epoch stamping, malformed
epochs, terminal acknowledgment debt, paused command cells, unsafe-side-effect
blockers, repeated reconciliation, and absence of fabricated completion facts.
The subprocess test proves local kernel lease recovery; it does not establish
external remote tool settlement or cross-platform native acceptance.
