# ADR 0045: EKO Binds Managed Conversation Creation and Aggregate Deletion

## Status

Accepted

## Context

The latest framework refuses legacy transcript writes and aggregate deletion when a `ConversationStore` and `RuntimeStateStore` advertise managed persistence. EKO already owns conversation identity guards, workspace generations, aggregate deletion tombstones, and separate runtime conversation IDs. A raw `ensure_conversation` or `delete_persisted_conversation` call no longer establishes the required epoch and retry identity.

## Options

1. Keep legacy calls and ignore managed failures. Rejected: conversations can be visible without a valid projection epoch, and deletion can lose its ABA fence.
2. Add another EKO transcript/deletion store. Rejected: it duplicates framework and existing aggregate authorities.
3. Adapt the existing EKO identity guard and tombstone to the framework managed APIs (chosen).

## Decision

- Under the existing conversation identity lock, EKO uses `ensure_projection_epoch` as the sole managed create/ensure operation before publishing a turn. A tombstoned or conflicting epoch is a failed admission, not an implicit recreation.
- GUI branches, Side first-turn snapshots, ordinary TUI fork/rewind/clear/resume, and REPL resume/reset/new/undo use one application coordinator to import canonical rows, CAS a matching runtime cursor, and hydrate the Agent. Direct replacement commands hold the existing conversation identity lock and suspend turn admission until hydration settles. A committed import with no checkpoint is recovered only by reconstructing and replaying the Store's durable locator and exact rows; direct retries first settle that debt rather than advancing another epoch. EKO validates checkpoint history before committing a new import.
- Rename uses the framework's revision-fenced metadata update only after any import debt is repaired. Cross-workspace move strictly proves the destination identity absent under ordered source/destination admission, imports and hydrates it, maps hidden source rows to new destination IDs using a durable epoch-bound visibility transfer, then retires the source through EKO aggregate deletion. A destination with a retired runtime generation is rejected; a return move requires a new conversation identity. A durable Prepared/TargetImported/SourceRetired handoff intent is replayed during startup without importing a second destination epoch; after follow-up admission, a completed receipt binds the source epoch and preserves exact retry results across response loss. If the source identity later reappears, an old receipt fails closed rather than reporting false success. Pending intents fence ordinary turn, replacement, creation, and deletion admission on both sides, including after a caller error. Destination admission stays closed through source retirement or controlled rollback; ambiguous source retirement retains the destination and reports debt.
- Side Conversation internal delivery uses AgentRouter as the delivery authority and the existing EKO conversation visibility file for durable pre-effect intent and exact committed row IDs. GUI hides those IDs in the primary view and labels them in the Side view; identical later user text remains user-authored. Visibility settles before Router terminal, so a persistence failure leaves the non-terminal frontier available for recovery. Owner-loss recovery settles any framework transcript projection first; if no row was committed, it records empty visibility before the unknown terminal. Side snapshots require matching transcript revisions and settled visibility on both sides of the read. The canonical managed transcript is never rewritten for UI metadata.
- Before the aggregate deletion crosses `ConversationCommitStarted`, EKO prepares `ManagedConversationDelete` from the exact live epoch and durably records it in the existing deletion tombstone. Every retry submits the same request; a legacy tombstone without that identity cannot invent an epoch after the commit boundary.
- Root deletion requires a completed framework scope-retirement receipt. EKO then removes each separate runtime conversation transcript returned by that receipt, using an epoch-fenced delete if it is managed. Only after all descendants settle does the existing RuntimeState deletion step advance.
- EKO's `task_execute` tool keeps only a weak reference to its owning Agent, avoiding an Agent-to-tool-to-Agent cycle. Once accepted work drains, workspace shutdown removes layered-memory tool and context-promoter projections that otherwise retain the retired manager; the Memory journal remains authoritative. A host's shared shutdown future also retains only its settled result, not a strong self-reference.

## Consequences

- Aggregate deletion remains the single EKO coordination authority; framework managed transcript and runtime-state journals retain their own CAS semantics. If target rollback commits but clearing the handoff intent fails, boot recovery recognizes the target tombstone and retires that abandoned intent rather than re-importing a new epoch.
- Old incomplete tombstones without a managed request remain explicit recovery debt rather than being rebound to a newly recreated conversation.
- Framework examples and `echo-website` do not consume EKO's deletion tombstone and need no update.
