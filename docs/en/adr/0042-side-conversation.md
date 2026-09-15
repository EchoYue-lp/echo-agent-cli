# ADR 0042: One-Level Side Conversations Reuse Conversation and Agent Routing Authorities

## Status

Accepted

## Context

EKO needs a quick way to fork committed context into a side discussion that can run and continue independently without blocking the primary conversation. The previous GUI `branch_conversation` served edit/regenerate only, while TUI `/fork` copied runtime messages into an unrelated conversation. They had no shared relationship, depth, model, or recovery contract.

The application already has a file-backed `ConversationStore`, conversation-scoped `AgentPool`, durable `ConversationInputService`, `ForegroundTurnControl`, `ChatEventLog`, `ConversationDeletionService`, and AgentRouter groups/inboxes. A dedicated Side Conversation store, journal, mailbox, or executor would duplicate those authorities.

Claude Code's documented Fork is also a Subagent that inherits the parent context and supports inspection, follow-up, and stop. EKO adopts the independent context and one-level topology, but keeps the child durably addressable in the navigation tree and does not automatically append its result to the primary transcript.

## Options

1. Add a SideConversationStore and dedicated runtime. This duplicates transcript, execution, recovery, and cancellation authorities.
2. Represent every side discussion as a formal TaskRun/PlanTask SubagentRun. This incorrectly couples free-form multi-turn conversation to DAG lifecycle.
3. Compose the existing Conversation, AgentGroup/AgentRouter, and AgentPool authorities behind one app-core service exposed only through GUI/Tauri (chosen).

## Decision

1. A Side Conversation is a one-level, addressable Subagent product role relative to the primary Agent. It is not a formal PlanTask attempt and cannot create another Side Conversation.
2. `ConversationStore` owns child metadata and a copied committed transcript. The current `StoredMessage` contract is linear and has no message DAG, so creation copies committed messages and removes source-conversation UI identities.
3. Each child uses one existing `AgentGroup`: the primary conversation is leader, the child is the only member, and typed metadata stores the idempotency hash, retryable initial prompt, synchronous launch error, model, snapshot boundary, and viewed marker. `groups.json` remains the only relationship authority.
   Side groups are omitted from the generic Agent group list, and generic update/delete operations reject their IDs so callers cannot bypass Side Conversation deletion and title synchronization. Side metadata updates mutate the latest record while holding the existing groups file lock; model, launch error, title, and viewed updates cannot overwrite each other with stale full-record writes.
4. Child and group IDs are deterministic over workspace, parent conversation, and request ID. Repeating the same request/payload returns a duplicate receipt; changing payload under the same request ID fails explicitly.
5. Children use the existing conversation AgentPool, input frontier, foreground turn, ChatEventLog, and deletion service. Tauri starts the first turn through the complete GUI chat driver with stable input/message identity, preserving the GUI sink, HITL, Browser approval, cancellation, and replay. Parent/child identity guards remain owned until the first GUI foreground lease is registered, so deletion cannot reopen a relation-less conversation in the gap after create. AgentRouter remains responsible only for internal Agent-to-Agent messages.
6. AgentGroup metadata is the durable child-model authority; AgentPool keeps only a rebuildable projection. Changing it retires the cached child Agent. An unavailable model fails explicitly.
7. Children retain the complete ordinary Agent tool surface. Side Conversation creation exists only in GUI/Tauri, and app-core admission rejects a Side parent before any side-creation effect, so the one-level rule does not disable Task or ordinary Subagent tools.
8. Internal Agent messages stay in the canonical model transcript for recovery, but their existing `attachments_json` projection is marked `internal_agent`. Primary views filter them by default; Side Conversation views retain them. Internal delivery waits for a separate cold turn.
9. Tauri exposes the converged conversation surface only: `list_conversations` includes an optional relation; `create_conversation` accepts `primary|side`; `update_conversation` accepts `rename|side_model|side_retry|side_viewed`; existing get/delete/cancel/send operations remain authoritative. No parallel `side_conversation_*` CRUD is retained.
10. GUI renders Workspace -> primary -> Side Conversation with status and unread state, and reuses the complete Agent timeline/composer. Side Conversation is a GUI layout capability that depends on a sidebar and parallel views, so only Tauri exposes it. TUI, CLI/JSONL, and channels add no dedicated commands, events, or wire contracts and retain their existing ordinary conversation, Subagent, and `/fork` behavior.

## Failure and Recovery

- Snapshot failure rolls back a newly created child. Relationship failure deletes only a child created by that attempt.
- If the child is durable but first-turn launch fails, the child, initial prompt, and `launch_error` remain durable. GUI retries use stable ConversationInput/message identity, return an existing active or terminal first turn, clear the error on success, and do not execute the input twice.
- Relation creation and parent cascade deletion share parent/child identity guards. Deleting an active child cancels and awaits its terminal before aggregate deletion, and the parent receipt aggregates every child cleanup debt.
- The create/retry receipt carries the stable initial prompt identity so a slow first turn is immediately visible in the selected timeline. Completed transcript recovery reconstructs the same identity from the snapshot boundary and initial prompt, preventing duplicate projection after a fast completion or lost response. Status polling uses a list generation independent from transcript loading, and a visible child advances its durable viewed marker instead of becoming spuriously unread after completion.
- A missing child remains a degraded relation and is never rebound by name.
- Child deletion completes existing aggregate cleanup before deleting the relationship. Parent deletion removes children first, and GUI confirmation shows the cascade scope.
- AgentRouter receipts continue to distinguish persisted, drained, and terminal delivery.

## Consequences

- No `echo-agent` public API changes; this is an EKO application composition.
- EKO gains one app-core Side Conversation service, typed AgentGroup metadata, child-local AgentPool projection, converged Tauri requests, a GUI adapter, and generated TypeScript contracts.
- Cross-surface parity still governs core Agent capabilities. Pure layout and window-composition affordances may remain surface-specific; this exception cannot justify removing task, Subagent, tool, HITL, memory, or attachment capabilities from TUI, CLI, or channels.
- Framework examples do not change because no public framework contract changed.
- `echo-website` does not publish the current EKO application UI or this application-only API, so no website update is required.
- The complete behavior is defined by `docs/supreme/specs/side-conversation/design.md`, including the correction that current main has neither a `ConversationDomainData` type nor a message DAG.
