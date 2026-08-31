# ADR 0008: Bounded TaskRuntime Query Projections

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
