# ADR-0024: Generation-bound hot-memory projection and shared reflection

## Status

Accepted and implemented, 2026-08-29.

## Context

EKO previously reconstructed `MemoryLayerManager` instances in several surfaces
and pushed refreshed hot-memory text into primary and pooled Agents after a
write. That created two authorities: the durable layered-memory store and a set
of surface-local prompt copies. A write originating inside a live Agent could
also wait for that same Agent's execution/write lock while trying to refresh
its context. Workspace focus changes and delete/recreate transitions made those
copies ambiguous because they did not carry the workspace generation that had
accepted the write.

The reusable framework already provides `Store`, `MemoryLayerManager`,
`EvolutionObserver`, and pre-model context projection primitives. Workspace
identity, GUI/TUI/CLI/channel parity, EKO review policy, and typed product
receipts are application concerns. No new framework architecture API is needed.

## Evidence and industry research

This decision checked three independent official implementations:

- OpenAI Codex at fixed commit
  `cdde711fac008cd4e1115603ead713cf23b1a580` keeps skill load outcomes in a
  shared manager cache keyed by effective inputs and bypasses that cache only
  on explicit force reload. See the official
  [SkillsManager source](https://github.com/openai/codex/blob/cdde711fac008cd4e1115603ead713cf23b1a580/codex-rs/core-skills/src/manager.rs#L51-L208).
- Claude Code's official
  [Hooks reference](https://code.claude.com/docs/en/hooks#add-context-for-claude)
  injects `additionalContext` into the next model request without presenting it
  as a chat message; `SessionStart` refreshes that context on startup/resume.
- LangGraph's official
  [persistence guide](https://docs.langchain.com/oss/python/langgraph/persistence)
  scopes checkpoint snapshots to a `thread_id` and separates them from durable
  cross-thread store data.

The common pattern is an identity-scoped durable authority plus an immutable
runtime snapshot consumed at a model boundary. EKO applies that pattern to its
local workspace memory without importing another product's storage model.

## Options considered

### Refresh every Agent after each write

Rejected. It performs per-Agent work, can self-deadlock when the writer is the
live Agent, and lets existing/future Agents observe different bytes.

### Keep surface-local managers and refresh helpers

Rejected. It duplicates the workspace authority, permits cross-workspace
drift, and makes `/reflect`, evidence writes, TaskRuntime memory, and agent tools
settle differently.

### Add an EKO workspace-generation API to the framework

Rejected. EKO workspace ABA identity, UI receipts, reflection commands, and
local product lifecycle are application policy. The framework primitives are
already sufficient.

### One generation lease plus one shared projection source

Accepted. Each `ReviewIntegration` owns one lazily initialized
`Arc<MemoryLayerManager>` and one `HotMemoryProjectionSource`. Every primary,
existing pooled, and future pooled Agent consumes that same source at the
framework pre-model safe point.

## Decision

1. A `ReviewGenerationLease` pins the authority scope, workspace generation,
   memory store, generation-bound mutation observer, and shared manager. No
   surface constructs a second manager.
2. Successful mutation batches settle exactly once. Settlement reads rendered
   hot memory once outside the async executor, computes a deterministic SHA-256
   content revision, and atomically publishes one immutable snapshot. It never
   waits for an Agent or enumerates pooled Agents.
3. `EkoContextProjector` reads the shared source at every pre-model safe point.
   This covers the primary Agent, existing pooled Agents, and Agents created
   after publication with byte-identical content.
4. Dirty epochs coalesce work but are not public revision identity. The receipt
   exposes authority scope, workspace generation, content revision, `changed`,
   settled/degraded status, current primary/pool/future bindings, pending
   revision, and error. A degraded publication does not roll back a durable
   memory write; it retains repair debt for the next settlement.
5. Workspace/application bootstrap must publish the initial snapshot before
   first admission. Missing bindings or read failure reject bootstrap instead
   of silently starting with empty memory.
6. Review shutdown retires projection targets and advances the generation
   fence. A stale lease cannot publish into a retired workspace. Pool/future
   flags are derived from the current weak pool lifetime, not sticky booleans.
7. Agent-tool writes settle when the retained run receipt releases. Evidence,
   TaskRuntime, Dreaming, explicit remember/forget, and interactive mutations
   use the same lease and settlement contract. Warm-only no-op batches do not
   trigger a hot projection read.
8. Framework commit `c4be2a9` completes the existing `EvolutionObserver`
   contract by notifying hot-memory deletion. This is a prerequisite for
   forget-driven dirty tracking, not a new workspace/projection architecture
   API.
9. `/reflect` is an app-core service: it reads the scoped transcript, asks the
   configured model for a concise durable reflection, writes through the same
   manager, and returns the same typed projection receipt to GUI, TUI, CLI,
   JSONL, and channel adapters. The old CLI-only `PROJECT.md` writer is removed.
10. EKO continues to use local file/in-memory authorities. This decision does
   not introduce SQLite.

## Consequences

Memory context becomes safe-point consistent without per-Agent I/O or lock
cycles. Workspace A/B generations cannot share managers or dirty signals, and
future pooled Agents see the latest committed snapshot automatically. Durable
writes can complete while projection repair is degraded, but that debt is
explicit and retryable rather than inferred from logs.

The application now owns a small amount of generation and receipt plumbing.
That is intentional EKO product policy. The framework remains reusable and
gains only the missing hot-delete callback on its existing observer contract;
no new framework workspace or projection API is introduced.
