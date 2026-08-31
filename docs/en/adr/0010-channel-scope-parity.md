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

## Consequences

Every surface preserves sender isolation and exact root/current-turn control.
No channel-local Agent, TaskRun, store, or foreground registry is introduced.
