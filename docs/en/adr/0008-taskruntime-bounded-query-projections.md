# ADR 0008: Bounded TaskRuntime Query Projections

## Status

Accepted

The finite GUI history adapter is implemented in the task-list branch; Rust, generated-wire, high-data and native qualification are recorded separately.


## Context

TaskRuntime already uses an append-only `events.jsonl` journal and framework
checkpoint primitives, but Todo, Artifact, and Requirement/Evidence queries
scanned the complete history. GUI latency therefore grew with 10k/100k event
history, and projection degradation had inconsistent meanings.

## Decision

Keep the journal as the only commit authority. Fold bounded Todo metadata,
latest summary, and completion evidence into the existing checkpoint. Maintain
unbounded Artifact and Review history as rebuildable incremental segments with
source cursors. Distinguish write acceptance from projection freshness and
return typed degraded receipts.

## Consequences

SQLite and a second read-model store are not introduced. Checkpoints and
segments are disposable accelerators; pruning never changes durable event
meaning. All surfaces consume the same bounded projections.

## Alternatives

The original decision rejects repeated full-history scans and a second SQLite/read-model authority. It keeps the existing journal/checkpoint and disposable query projections. The GUI history adapter follows that same choice rather than adding an inventory store.

## Finite GUI run and event reads

`list_task_runs` returns original `TaskRun` headers in pages of at most 20, ordered by creation time and run ID descending. A caller-owned opaque continuation binds the original workspace/product-data generation, normalized status filter, size and ordering positions. It carries no server session or durable state. The original provenance/committed-plan predicate is shared with the conversation's latest-task lookup; diagnostic route text does not decide visibility.

The new read path retains only a page plus one look-ahead header, but still enumerates run IDs and recovers their original authorities. It does not claim constant scan cost or an index. A failed candidate is reported, rather than silently omitted as a complete successful page. Pages read live facts, not a multi-request transaction snapshot; status/plan changes and new runs can require explicit refresh.

The original event IPC uses `query_events_bounded` and returns at most 256 events, the lossless string `next_after_sequence`, and `has_more`. Gaps, invalid positions and recovery failures remain errors; retained frontend events are not the transport bound. Unread history stays visible until subsequent pages are observed. Existing journals, write acceptance, control admission and physical settlement are unchanged. Pause/cancel/resume/retry display their existing typed receipts; a no-op or an accepted request is not execution completion.

Reference: [GitHub REST pagination](https://docs.github.com/en/rest/using-the-rest-api/using-pagination-in-the-rest-api) uses bounded server responses and explicit continuation. EKO reuses that pattern over its existing local IPC, without importing the remote service's limits.

Historical inspection uses at most one additional presentation snapshot in the same Zustand store. It does not replace the current chat’s active run, Todo count, failure notice or message association. Task-panel actions carry their rendered run ID; conflict Continue uses the original admitted workspace/run and typed receipt, never a selected historical status. Late results cannot clear a successor prompt or update another presentation.

Projection-only orphan directories remain non-runs under the original journal authority rule. The finite inventory does not infer a lost TaskRun from a projection or add a registry; non-NotFound I/O, malformed journals and recovery errors remain explicit.

### Captured generation for GUI task controls

All existing task-panel mutations (pause/cancel/resume/retry, Goal, continuation budgets, graph edits, recovery decisions and requirement skips) carry the captured `workspace_generation`, together with workspace and run identity. `AppState::workspace_control_for_generation` validates the token on the same existing control receipt returned to the command; its runtime clone retains that host lease through the actual operation. The command does not validate one host then resolve another by workspace id. This prevents a delayed action for an earlier project-root revision from mutating a replacement incarnation. Continue uses the scope captured by the original input admission, while detail controls use the rendered target. This is target identity, not an Agent permission gate.

An explicit history selection also retains its one bounded snapshot when that run is currently displayed in chat. A later chat run cannot replace the selected detail. Detail/event request generations settle only their own busy indicators; following chat or selecting another history transfers those indicators without allowing an older callback to clear the successor request.

### Durable queued Steer replacement

Queued Steer editing is an application consumer operation over the existing ConversationInput journal. A replacement uses the existing `Persisted` fact with the same input id, the next identity revision, a new payload hash, and the existing expected queue revision. The reducer retains the original queue position and first sequence. Only dispatchable inputs without an active attempt may be replaced; stale or active identities return a typed validation error. No second queue store or CRUD authority is introduced.
