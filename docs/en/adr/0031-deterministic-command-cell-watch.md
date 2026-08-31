# ADR 0031: Deterministic Command-Cell Watch

## Status

Accepted

## Context

EKO previously registered an `awaiter` Subagent whose only job was to call the
framework `wait` tool until one background command cell became terminal. EKO
then re-read the typed cell state before writing `AwaiterResultReady`. The model
summary was explicitly non-authoritative, yet the path still required a model,
prompt, Subagent attempt, provider availability, and process Subagent permit.

Framework ADR 0025 now provides a retained deterministic watcher over the
existing `CommandCellRegistry` authority.

## Decision

1. Delete the built-in `awaiter` definition and all model dispatch, provider
   summary, `BackgroundSubagentHandle`, and Subagent admission from `watch_cell`.
2. `watch_cell` acquires `CommandCellWatcher` before EKO's durable exact-owner
   read, preserving the existing snapshot/live-retention race closure.
3. EKO keeps `CommandCellWatchReceipt`, generation idempotency, bounded active
   watches, exact interrupt, Ready/delivery/ack facts, boot repair, foreground
   steer, next-turn projection, and all GUI/TUI/CLI/channel renderers.
   Watch admission and tracker spawn are linearized with shutdown in the same
   runtime-state lock, so no observer can appear after shutdown's join cut.
4. `CommandCellWatchResult` contains only its durable receipt and the projected
   typed `BackgroundCellState`. There is no provider-derived status or summary.
5. Interrupting a watch cancels its observer intent but does not stop the
   command. The configured framework watcher continues short-polling until the
   real terminal so accepted result delivery is not lost.
6. Current development event names are `command_cell_watch_ready`,
   `command_cell_watch_delivery_started`, and
   `command_cell_watch_acknowledged`; no legacy aliases are retained.

## Consequences

- The primary Agent can continue while the deterministic watch runs, without
  spending model tokens or occupying Subagent capacity.
- Cell phase, terminal cause, exit code, output, and artifact state have one
  authority from launch through delivery.
- EKO still owns local conversation/workspace identity and durable UI delivery;
  these product policies do not move into the framework.
