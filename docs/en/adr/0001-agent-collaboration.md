# ADR 0001: Agent Collaboration and EKO Capability Design

> Status: Proposed
>
> Date: 2026-08-24; implementation evidence review: 2026-08-25

## Context

Codex collaboration combines independent task threads, a host-maintained task
directory, exact-address messaging, event waiting, and coordination policy.
EKO should provide the same product shape without sharing hidden context or
creating a second task executor.

## Decision

EKO adopts independent Agent sessions, a shared directory index, exact target
messages, event cursors, and explicit coordination tools. Generic Agent,
Subagent, DAG, cancellation, and event primitives remain in `echo-agent`;
workspace policy, product identity, and surface projections remain in EKO.

## Consequences

An App task, an Agent tree root, a Subagent, an Agent instance, a turn, a Task,
and a Goal have distinct identities. Coordination must use bounded queries and
durable receipts, never polling or natural-language terminal inference.

This ADR is a capability reference and does not promise a private Codex
backend, shared prompts, arbitrary cross-session file access, or a second
Task/Plan/Subagent authority.
