# ADR 0048: EKO context compression product policy

## Status

accepted

## Context and alternatives

Framework ADR 0081 follows Codex's bounded user material and reconstructed
initial context, and Pi's recent-token allowance and atomic tool boundaries.
EKO already owns TaskRuntimeStore, goal/recovery projections and the app-core
manual compression service. Unlimited raw user history cannot fit a bounded
window; a second goal store or application selector would duplicate authority.

## Decision

EKO defaults to `compress_window: 0`: 25% of the live model window, capped at
20K recent tokens. Positive values explicitly select the legacy message cap.
Adaptive keeps its own levels. `/compress`, `/compact`, TUI and GUI use the same
installed policy and optional focus, replacing ineffective 6/12-message claims.
The GUI has an optional focus field; TUI accepts command arguments.

The app-core foreground/AgentPool/workspace owner passes cancellation to the
framework and awaits settlement. Cancellation before transform commit keeps
context; after success the journal safe point survives caller drop/cancellation.
Goal/recovery projections come from TaskRuntimeStore. The latest four recorded
user steer excerpts retain their complete journal-bounded text in chronological
order, rather than receiving a second 200-character cut. Projection refresh is
replaceable and scope exit removes it.

ConversationStore, ContextManager, RuntimeStateStore, TaskRuntimeStore and
Trace/ChatEventLog continue to own transcript, active context, checkpoint,
goal/execution facts and diagnostic/events respectively.

## Impact, rollback and validation

Remove the ineffective `keep_messages` request option. Existing framework Rust
constructors and explicit positive EKO windows remain available. Reverting this
slice restores the old adapters without deleting transcript or task journals.
Framework delivery precedes application mainline integration.

Validation covers focus parity and one journal event on all four surfaces,
pre-commit cancellation, caller-drop safe points and journal failures. Actual
compression must preserve the goal and the end of recorded constraints without
accumulating projections. Mock tests do not establish native UI/provider acceptance.
