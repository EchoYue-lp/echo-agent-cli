# ADR 0047: Automatic Agent collaboration

## Status

Accepted

## Context

EKO already provides model-facing Agent control tools, durable conversation
delivery, correlated replies and exact TaskSubagent control. A GUI dialog
duplicated these mechanisms as manual recipient selection, message delivery and
group administration. The user explicitly requires AI to organize collaboration;
Sidechat and Forkchat are the user's ways to participate in independent exploration.

The user's Codex interaction reference and Claude Code's official
[agent-team message delivery](https://code.claude.com/docs/en/agent-teams) support
tool-driven coordination. EKO keeps its own durable address and lifecycle
authorities rather than requiring users to maintain them through a GUI.

## Alternatives

1. Keep manual message/group administration: makes users coordinate the Agents.
2. Add an activity-management panel with secondary manual controls: still exposes
   Agent coordination as a separate user-managed product workflow.
3. Remove the collaboration management surface and retain automatic tools and
   runtime: matches the user's explicit product boundary.

## Decision

Choose option 3. Remove the standalone GUI collaboration dialog, group editor,
chat entry point, unused frontend API/types and their Tauri commands. Do not add
an activity query, inbox observer or another management surface. AppState,
AgentControlService, AgentRouter and TaskRuntime retain their existing model
tools, delivery/settlement and return-result mechanisms. TUI/CLI/channel controls
and reusable framework APIs are outside the GUI removal.

Side Conversation creation/navigation and ordinary GUI Fork remain explicit user
actions. Existing conversation, task/Subagent output and HITL surfaces retain
their behavior. GUI removal changes no durable data or message semantics.

## Consequences and validation

The removed UI is intentionally retired, not behavior-equivalent to another
manual GUI. Core coordination is preserved and checked separately through its
existing runtime/control tests. Check Sidechat and Fork interactions, frontend
compilation/tests, Tauri registration and full applicable Rust gates. Framework
examples and echo-website do not consume the removed GUI API; the Rust 1.99
atomic-operation rename is a separate compiler lint repair with unchanged
ordering, overflow and lifecycle behavior.
