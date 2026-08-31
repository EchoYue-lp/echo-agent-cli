# ADR 0026: Typed AgentRouter Ledger

## Status

Accepted

## Context

AgentRouter used to encode `AgentMessage` into a framework envelope containing
`String` route, JSON `Value` payload, and metadata, then rebuild an EKO
projection on reads. That shape made the application look like it owned a
second message and record model even after lifecycle authority had moved to the
framework.

## Decision

Use the documented framework facade directly:

```text
DeliveryLedger<Journal, AgentAddress, AgentMessage>
```

`AgentAddress` implements the framework's `DeliveryRoute` contract. The
framework `DeliveryRecord<AgentAddress, AgentMessage>` is the sole durable
record and is returned by AgentRouter status queries. EKO keeps only endpoint,
workspace, wake, retirement, group, and surface policy. It does not define an
AgentRouter projection reducer or a source-named conversion from framework records.
Lifecycle commands are represented by the framework `DeliveryTransition`; EKO
does not mirror them in an application-side settlement enum.

The prior `AgentInboxEvent` wire and `delivery-ledger.checkpoint.json` bridge
are deleted from the active code path. This repository is in development and
does not promise compatibility with those local files; a fresh data root is
required after upgrading.

## Consequences

- The public EKO status response now contains the typed framework record fields:
  `route`, `payload`, `phase`, lifecycle timestamps, and retention metadata.
- Framework lifecycle phases `effect_started` and `deferred` are visible to
  clients instead of being flattened into `claimed` and `persisted`.
- The framework and application have one delivery reducer and one projection;
  GUI, TUI, CLI, and channels consume the same result.
