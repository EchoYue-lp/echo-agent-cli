# ADR 0016: Unified Agent Collaboration Control Plane

## Context

Models need bounded discovery, inspection, messaging, follow-up, waiting, and
interrupt control for both Conversation Agents and TaskRun Subagents. Existing
router inboxes, Subagent control, and TaskRuntime journal already provide the
authorities; another mailbox or store would duplicate them.

## Decision

Add an app-core routing service with discriminated `ConversationTarget` and
`TaskSubagentTarget`. Register `agent_list`, `agent_inspect`, `agent_message`,
`agent_followup`, `agent_wait`, and `agent_interrupt`. Validate revision,
attempt, generation, and workspace identity before delegating to existing
services. Bounded event and exact-attempt queries are provided by TaskRuntime
and router cursors.

## Consequences

All GUI, TUI, CLI/JSONL, channels, and pooled Agents share one schema and
authority. The service does not own a task graph, mailbox, retry loop, terminal
reducer, or unbounded history scan. Conversation existence comes from the
workspace ConversationStore, not a router-only target.
