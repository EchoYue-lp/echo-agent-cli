# ADR 0011: Boot and Agent Inbox Recovery Authority

## Context

TaskRun, chat command cells, deterministic CommandCellWatch work, and cross-session Agent messages
must converge after restart. Earlier code cached transient recovery failures,
held workspace locks across file I/O, launched background work twice, and
treated mailbox acceptance as consumption.

## Decision

TaskRun boot recovery is a success-only singleflight owned by the store. AppState
owns ordinary conversation continuation; BackgroundTaskService owns global
background launch. Running cells become typed interrupted or paused facts.
Agent delivery records effect start before side effects and records accepted or
drained only after framework receipts. Owner-loss without a typed terminal is
`outcome_unknown` and is never replayed automatically.

## Consequences

Framework steer receipts and segmented journals remain the generic authority.
EKO owns workspace scanning, attended policy, backoff, and UI projection, with
bounded I/O and stable identity across restart.

Command-cell observation now uses framework `CommandCellWatcher`; EKO retains
only durable owner validation, Ready/delivery/ack repair, and surface handoff.
ADR 0031 supersedes the earlier model-driven observer mechanics.
