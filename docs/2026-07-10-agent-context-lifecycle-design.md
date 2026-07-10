# Main Agent / Subagent Context Lifecycle Repair

## Goal

Fix the reviewed regressions without moving EKO-specific TaskRuntime state into
the generic `echo-agent` framework:

- keep exactly one current TaskRuntime recovery capsule visible to the main
  agent at every model-call boundary;
- make Fork subagent invocations start from clean task state;
- keep canonical compression metadata internally consistent;
- report context and subagent usage accurately without transcript-wide
  per-token work.

## Industry references

- Claude Code externalizes durable plans/TODOs and re-injects project context
  around compaction rather than preserving every historical tool result.
- Codex compacts at pre-turn and mid-turn loop boundaries and treats the
  replacement context as the new canonical input.
- LangGraph provides a pre-model hook for trimming/injecting state, while its
  subagents start with fresh state by default unless persistence is explicitly
  requested.

EKO follows the shared pattern: file-backed TaskRuntime remains authoritative;
model-visible recovery context is a replaceable projection generated at the
model-call boundary.

## Framework responsibilities (`echo-agent`)

1. Add a generic pre-model context projection hook. It receives run-scoped
   context and can replace a tagged projection immediately before
   `ContextManager::prepare`. The framework knows nothing about EKO runs,
   plans, or TaskRuntimeStore.
2. Make Fork dispatch clear prior worker conversation state while preserving
   canonical/system configuration and explicitly inherited parent context.
   Reusing the worker implementation must not imply reusing the previous
   worker task transcript.
3. Make canonical re-injection idempotent and update compression checkpoints
   from the final post-injection context.
4. Expose generic protected-message diagnostics from ContextManager so the GUI
   does not duplicate marker strings.

## Application responsibilities (`echo-agent-cli`)

1. Register a TaskRuntime projection provider on main agents. Before every LLM
   call it reads the current run ID and file-backed store, removes any previous
   recovery capsule, and inserts at most one current capsule. With no active
   plan/todos, it removes the stale capsule and inserts nothing.
2. Stop embedding protected capsules into `task_create` and `plan_execute`
   ToolResults. Tool results remain ordinary conversational evidence.
3. Keep `[task_context]` scoped to one fresh Fork invocation rather than
   accumulating it in reusable worker history.
4. Report isolation as requested/observed state. A fallback must never be
   displayed as successful worktree isolation.
5. Parse evidence only from credible file-reference forms and support
   `path:line`, `path:start-end`, and Markdown links without accepting generic
   slash-containing prose.

## Frontend behavior

1. Use provider-reported prompt tokens as the context baseline and add only
   unsent draft/attachment cost. Before the first report or immediately after
   compression, show draft-only/unknown state rather than rebuilding backend
   context from the full rendered transcript.
2. Remove the full `messages` subscription and transcript scan from
   `ChatInput`, avoiding work on every streamed token.
3. Count only canonical LLM usage events and filter subagent diagnostics to the
   active conversation/run.
4. Display missing runtime-contract fields as unknown/absent, never as invented
   values.
5. Use backend protected-message diagnostics derived from actual registered
   markers.

## Error handling and compatibility

- A projection-provider failure is logged and leaves the previous model-visible
  context cleared rather than injecting stale state.
- No SQLite state or schema is introduced.
- Existing public framework behavior remains unchanged when no hook is
  configured.
- Fresh Fork context is the intended isolation semantic; persistent subagent
  conversations require an explicit future mode rather than accidental
  singleton history.

## Testing

- Framework regression tests: idempotent canonical injection, final checkpoint
  counts, projection replacement/removal, and two sequential Fork tasks not
  sharing prior task context.
- Application tests: stale capsule removal, single capsule after repeated task
  mutations, evidence-path edge cases, and requested/fallback isolation
  metadata.
- Frontend tests or extracted pure helpers: provider baseline plus draft,
  compression reset, canonical usage filtering, active-conversation filtering,
  and unknown contract fields.
- Run complete framework crate verification, application workspace tests plus
  GUI feature checks, frontend typecheck/build, formatting, and feature matrix.
