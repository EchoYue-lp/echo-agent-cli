# ADR 0010: Sender-Scoped Channel Runtime and Exact Control

> Status: Accepted
>
> Date: 2026-08-25

## Context

Channel handlers are scoped by `(channel_id, conversation_id, sender_id)`, but
the application once keyed AgentPool only by channel and chat. Different group
members could therefore share Agent, transcript, TaskRun, and provider cache.

## Decision

Hash the structured three-part channel identity into a stable product
conversation. Hash that identity with each framework incarnation to derive the
AgentPool and checkpoint key. Product journal, TaskRun, router, UI, and
foreground use the stable identity; runtime and cache use the incarnation.
Reset rotates the runtime after closing old admission and waiting for exact
foreground/lease settlement. It never erases product history.
The framework exact-clear operation retires the incarnation checkpoint. EKO
then deletes that incarnation's separate transcript using its managed epoch;
neither step deletes the stable product conversation or another sender's data.

Product deletion uses EKO's tombstone to coordinate TaskRun, tool, chat event,
and artifact cleanup. With EKO's AtomicV1 conversation store and revisioned
runtime store, the authority commit calls
`delete_persisted_conversation_managed`. Before `ConversationCommitStarted`,
EKO persists the `ManagedConversationDelete`, including the conversation,
expected epoch, and request identity, in that tombstone. Normal execution,
retries after a lost acknowledgement, and restart reuse that same request;
they must not prepare a new request from a recreated conversation with the
same ID. The helper retires the scope's incarnation checkpoints. EKO requires
a `Completed` or `AlreadyCompleted` retirement receipt before continuing
incarnation transcript cleanup; conflicts or incomplete retirement do not
count as completed deletion and must not leave lineage behind.

## Consequences

Every surface preserves sender isolation and exact root/current-turn control.
No channel-local Agent, TaskRun, store, or foreground registry is introduced.
